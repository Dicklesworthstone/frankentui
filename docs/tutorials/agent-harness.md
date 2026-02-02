# Agent Harness Tutorial

This tutorial shows how to build a Claude/Codex style agent harness using ftui.
All examples are aligned with the current API in this repo and mirror the
reference harness code under `crates/ftui-harness/`.

If you want working code right now, start with:
- `crates/ftui-harness/examples/minimal.rs`
- `crates/ftui-harness/examples/streaming.rs`

## Prereqs

- Rust nightly (see `rust-toolchain.toml`)
- A terminal that supports basic ANSI (tmux and zellij are supported)

Run the examples:

```bash
cargo run -p ftui-harness --example minimal
cargo run -p ftui-harness --example streaming
```

## Part 1: Hello World Harness (< 50 LOC)

Goal: the smallest possible inline harness that runs and exits cleanly.

```rust
use ftui::prelude::*;
use ftui::text::Text;
use ftui::widgets::{Paragraph, Widget};

struct HelloHarness {
    message: String,
}

impl Model for HelloHarness {
    type Message = Event;

    fn update(&mut self, msg: Event) -> Cmd<Event> {
        if let Event::Key(k) = msg {
            if k.is_char('q') {
                return Cmd::quit();
            }
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let area = frame.bounds();
        let paragraph = Paragraph::new(Text::raw(&self.message));
        paragraph.render(area, frame);
    }
}

fn main() -> ftui::Result<()> {
    App::inline(
        HelloHarness {
            message: "Hello from ftui. Press q to quit.".to_string(),
        },
        1,
    )
    .run()?;
    Ok(())
}
```

Key concepts:
- `Model` with `update()` and `view()`
- `App::inline` for scrollback-preserving inline mode
- `Event` messages via `From<Event> for Event` (built in)

Layout cheat sheet (inline mode, UI pinned at bottom):

```
+--------------------------------------+
| status line (optional, 1 row)         |
+--------------------------------------+
| log viewer (scrolling UI region)      |
| ...                                   |
+--------------------------------------+
| input line (optional, 1 row)          |
+--------------------------------------+
```

## Part 2: Log Streaming (UI + scrollback)

Goal: show a scrolling log view in the UI region, and also write to the
terminal scrollback safely.

Notes:
- Use `LogViewer` to render log lines inside the UI region.
- Use `Cmd::log` to write to the scrollback region. It sanitizes by default.
- Use `Every` subscriptions for periodic updates.

```rust
use std::time::Duration;

use ftui::prelude::*;
use ftui::core::geometry::Rect;
use ftui::text::Text;
use ftui::widgets::log_viewer::{LogViewer, LogViewerState};
use ftui::widgets::StatefulWidget;
use ftui::runtime::{Every, Subscription};

struct LogHarness {
    log: LogViewer,
    state: LogViewerState,
    count: usize,
}

#[derive(Debug)]
enum Msg {
    Event(Event),
    Tick,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        Msg::Event(e)
    }
}

impl Model for LogHarness {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Event(Event::Key(k)) if k.is_char('q') => Cmd::quit(),
            Msg::Tick => {
                self.count += 1;
                let line = format!("[{:04}] stream tick", self.count);
                self.log.push(Text::raw(&line));
                Cmd::log(line)
            }
            _ => Cmd::none(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.width(), frame.height());
        let mut state = self.state.clone();
        self.log.render(area, frame, &mut state);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Msg>>> {
        vec![Box::new(Every::new(Duration::from_millis(200), || Msg::Tick))]
    }
}

fn main() -> std::io::Result<()> {
    let mut log = LogViewer::new(5_000);
    log.push(Text::raw("Streaming demo started"));

    App::new(LogHarness {
        log,
        state: LogViewerState::default(),
        count: 0,
    })
    .screen_mode(ScreenMode::Inline { ui_height: 8 })
    .run()
}
```

## Part 3: Interactive Input (TextInput)

Goal: accept keyboard input, echo it into the log, and exit on Ctrl+C.

```rust
use ftui::prelude::*;
use ftui::core::geometry::Rect;
use ftui::text::Text;
use ftui::widgets::input::TextInput;
use ftui::widgets::log_viewer::{LogViewer, LogViewerState};
use ftui::widgets::{StatefulWidget, Widget};

struct InputHarness {
    log: LogViewer,
    log_state: LogViewerState,
    input: TextInput,
}

#[derive(Debug)]
enum Msg {
    Event(Event),
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        Msg::Event(e)
    }
}

impl Model for InputHarness {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Event(Event::Key(k)) => {
                if k.ctrl() && k.is_char('c') {
                    return Cmd::quit();
                }
                if k.code == KeyCode::Enter {
                    let line = self.input.value().to_string();
                    self.input.clear();
                    if !line.is_empty() {
                        self.log.push(Text::raw(format!("> {}", line)));
                        return Cmd::log(line);
                    }
                }
                self.input.handle_event(&Event::Key(k));
            }
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let area = frame.bounds();
        let log_area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
        let input_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);

        let mut log_state = self.log_state.clone();
        self.log.render(log_area, frame, &mut log_state);

        self.input.render(input_area, frame);
    }
}

fn main() -> std::io::Result<()> {
    let mut log = LogViewer::new(5_000);
    log.push(Text::raw("Type and press Enter. Ctrl+C exits."));

    let mut input = TextInput::new();
    input.set_focused(true);

    App::new(InputHarness {
        log,
        log_state: LogViewerState::default(),
        input,
    })
    .screen_mode(ScreenMode::Inline { ui_height: 6 })
    .run()
}
```

## Part 4: Status Line and Spinner

Goal: add a status bar and a spinner that animates on ticks.

Notes:
- `Spinner` uses a `SpinnerState` that you can tick in `update()`.
- Use `LINE` frames if you want ASCII-only spinners.

```rust
use std::time::Duration;

use ftui::prelude::*;
use ftui::core::geometry::Rect;
use ftui::layout::{Constraint, Flex};
use ftui::text::Text;
use ftui::widgets::log_viewer::{LogViewer, LogViewerState};
use ftui::widgets::spinner::{Spinner, SpinnerState, LINE};
use ftui::widgets::status_line::{StatusItem, StatusLine};
use ftui::widgets::{StatefulWidget, Widget};
use ftui::runtime::{Every, Subscription};

struct StatusHarness {
    log: LogViewer,
    log_state: LogViewerState,
    spinner: Spinner<'static>,
    spinner_state: SpinnerState,
    ticks: usize,
}

#[derive(Debug)]
enum Msg {
    Event(Event),
    Tick,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        Msg::Event(e)
    }
}

impl Model for StatusHarness {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Event(Event::Key(k)) if k.is_char('q') => Cmd::quit(),
            Msg::Tick => {
                self.ticks += 1;
                self.spinner_state.tick();
                self.log.push(Text::raw(format!("tick {}", self.ticks)));
                Cmd::none()
            }
            _ => Cmd::none(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = frame.bounds();
        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Min(2)])
            .split(area);

        let ticks_text = format!("ticks: {}", self.ticks);
        let status = StatusLine::new()
            .left(StatusItem::text("MODEL: demo"))
            .center(StatusItem::text(&ticks_text))
            .right(StatusItem::key_hint("q", "Quit"));
        status.render(chunks[0], frame);

        let mut log_state = self.log_state.clone();
        self.log.render(chunks[1], frame, &mut log_state);

        // Render spinner at the far right of the status row
        let spinner_area = Rect::new(area.right().saturating_sub(2), area.y, 2, 1);
        let mut state = self.spinner_state.clone();
        self.spinner.render(spinner_area, frame, &mut state);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Msg>>> {
        vec![Box::new(Every::new(Duration::from_millis(100), || Msg::Tick))]
    }
}

fn main() -> std::io::Result<()> {
    let mut spinner = Spinner::new();
    spinner = spinner.frames(LINE).label("working");

    App::new(StatusHarness {
        log: LogViewer::new(2_000),
        log_state: LogViewerState::default(),
        spinner,
        spinner_state: SpinnerState::default(),
        ticks: 0,
    })
    .screen_mode(ScreenMode::Inline { ui_height: 6 })
    .run()
}
```

## Part 5: Inline vs Alt-Screen

Current runtime selection is fixed at startup:
- Inline mode: `App::inline(model, ui_height)` or `.screen_mode(ScreenMode::Inline { .. })`
- Alt-screen: `App::fullscreen(model)` or `.screen_mode(ScreenMode::AltScreen)`

There is no runtime API for switching screen modes mid-session yet. If you need
full-screen modal behavior today, spawn a separate fullscreen program or exit
and re-run in `AltScreen`. The planned behavior is to allow modal transitions,
but it is not implemented in the current runtime API.

Related docs:
- `docs/concepts/screen-modes.md`

## Part 6: PTY Child Process Capture (feature-gated)

Use PTY capture to keep subprocess output inside the one-writer path. The
reference helper lives in `crates/ftui-harness/src/pty_capture.rs` and depends
on `ftui-extras` with the `pty-capture` feature.

Example (from the harness helper, simplified):

```rust
#[cfg(feature = "pty-capture")]
fn run_tool(writer: &mut ftui::TerminalWriter<std::io::Stdout>) -> std::io::Result<()> {
    use ftui_harness::pty_capture::run_command_with_pty;
    use ftui_extras::pty_capture::PtyCaptureConfig;
    use portable_pty::CommandBuilder;

    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "printf 'hello from tool\\n'"]);

    let _status = run_command_with_pty(writer, cmd, PtyCaptureConfig::default())?;
    Ok(())
}
```

## Core Concepts (short and practical)

### One-writer rule
All bytes that affect terminal state must go through the runtime or
`TerminalWriter`. Do not `println!()` while an app is running.

See: `docs/one-writer-rule.md`

### Cursor contract
Widgets can set cursor position via `frame.set_cursor(Some((x, y)))` or by
using `TextInput` with `focused = true`. The runtime restores cursor state
after each present.

### Sanitization
`Cmd::log` sanitizes output by default. Do not pass untrusted bytes directly
into raw terminal output.

See: `docs/adr/ADR-006-untrusted-output-policy.md`

## Common mistakes and fixes

Wrong (writes directly to stdout):

```rust
println!("debug: {}", value);
```

Right (use structured logs or Cmd::log):

```rust
tracing::debug!(value, "debug");
// or from update(): Cmd::log(format!("debug: {}", value))
```

Wrong (blocking inside update):

```rust
fn update(&mut self, _msg: Msg) -> Cmd<Msg> {
    std::thread::sleep(std::time::Duration::from_secs(5));
    Cmd::none()
}
```

Right (use a background task):

```rust
fn update(&mut self, _msg: Msg) -> Cmd<Msg> {
    Cmd::task(|| Msg::Done)
}
```

## Next steps

- Read the reference harness app: `crates/ftui-harness/src/main.rs`
- Explore the examples: `crates/ftui-harness/examples/`
- Review screen mode trade-offs: `docs/concepts/screen-modes.md`
- Review one-writer guidance: `docs/one-writer-rule.md`
