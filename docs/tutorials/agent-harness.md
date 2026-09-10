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
cargo run -p ftui-harness --example streaming -- --exit-when-child-exits -- seq 1 10000
cargo run -p ftui-harness --example streaming -- --stdin --exit-when-child-exits -- cat
cargo run -p ftui-harness --example streaming -- --stdin -- cat
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
    App::new(HelloHarness {
        message: "Hello from ftui. Press q to quit.".to_string(),
    })
    .screen_mode(ScreenMode::Inline { ui_height: 1 })
    .run()?;
    Ok(())
}
```

Key concepts:
- `Model` with `update()` and `view()`
- `App::new(...).screen_mode(ScreenMode::Inline { .. })` for scrollback-preserving inline mode
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
- Use `Cmd::log` to write plain text to the scrollback region. It strips terminal controls.
- Use `Cmd::log_sgr_only` to retain ANSI colors while stripping cursor, title,
  clipboard, and terminal-mode commands. The writer resets style at line and UI boundaries.
- Use `Every` subscriptions for periodic updates.

The runnable `streaming` example submits every generated line to both `LogViewer`
and `Cmd::log_sgr_only`. Terminal logs accumulate when a scroll region is active
and log rows are available. The overlay fallback displays only the latest
width-clamped first line; no available log rows means no terminal log output.
With no arguments the example generates messages on a timer. A command after
`--` starts a real `ProcessSubscription`: stdout and stderr feed the log viewer
and plain-text terminal logs by default. Stderr lines have a `[stderr]` prefix. The
status records the child's exit code or signal and the total stdout/stderr line
count. Add `--exit-when-child-exits` to exit after logging that final status;
otherwise the chrome remains visible until you quit. This TUI example logs
the child's exit code; its own exit code reports whether the application ran
successfully, so it is not a shell command wrapper.

Use `--log-mode=sgr-only` before `-- COMMAND` to retain the child's SGR styling,
for example:

```bash
cargo run -p ftui-harness --example streaming -- --log-mode=sgr-only --exit-when-child-exits -- printf '\033[31mred diagnostic\033[0m\n'
```

The three modes are `sanitized` (strip escapes, the default), `sgr-only`
(retain bounded SGR styling and strip terminal commands), and `raw` (trusted
terminal commands allowed). The process status bar shows the selected mode.
`FTUI_AGENT_SHELL_LOG_MODE` sets the child-output default; an explicit CLI option
overrides it, including an invalid environment value. `--log-mode VALUE` also
works. Missing, invalid, or repeated options fail before the TUI and child start.
The CLI option requires a child command; the environment setting has no effect
on the generated demo. Arguments after `--` are passed literally to the child.

Only child stdout/stderr uses the selected policy. The on-screen log viewer,
input echoes, control feedback, and final status remain sanitized. SGR-only
resets styling at each delivered line boundary so it cannot carry into the next
line or chrome; SGR includes attributes such as conceal and inverse, not just
colors. Raw output can change cursor position, terminal modes, and styling;
it can disrupt the chrome and subsequent logs. Raw still uses the same bounded
UTF-8 line transport and line-ending normalization, so it is not byte-transparent.

The child-output viewer recognizes literal HTTP(S) URLs in records that contain
no C0/C1 controls. Their visible labels and targets retain the original Unicode,
query and fragment text. Whitespace, angle brackets and quotation marks delimit
tokens; leading prose brackets and trailing sentence punctuation stay outside
the target. Balanced parentheses within a URL are retained, while unmatched
closing prose brackets are excluded. Targets longer than 4,096 UTF-8 bytes,
empty authorities, backslash-containing targets and other schemes remain plain.
This is conservative prose recognition, not a full URL validator. Any original
control character, including an SGR or OSC escape, makes the whole record plain
after sanitization, so stripping escapes cannot manufacture a trusted target.

At most 32 occurrences per child record receive link metadata. The example uses
`LogViewer::retain_links_for_last_lines(200)` to remove targets from older records
while preserving their exact text and styles within the existing 10,000-record
history. This opt-in builder also applies to records already present; its default
leaves links intact, and increasing the limit cannot restore removed targets.
The example's `App::new(...).with_hyperlink_limit(200)` separately bounds distinct links
in each frame and retained registry slots to twice that limit. Excess links
remain visible as plain text. Input echoes, control/status messages and generated
demo lines do not receive automatic links. These links belong to the viewer;
terminal log commands retain the selected trust mode and do not gain automatic
OSC8 links. Unsupported hyperlink capabilities produce plain viewer output.
Wrapped or search-highlighted lines can lose span links, and compact interactive
chrome with no viewer has no visible links. In-memory view tests do not establish
physical terminal clicking or behavior across terminal hosts.

`--exit-after-ms=N` ends the whole session after a nonnegative number of
milliseconds from model initialization. `FTUI_AGENT_SHELL_EXIT_AFTER_MS` supplies
the default; the CLI overrides it, including invalid environment values. Zero
quits during initialization without starting a child. The deadline survives F5,
child exit and paused generated output. It is checked on accepted model updates
and the existing timers (250 ms for a child, 50 ms for generated output), so slow
updates or terminal writes can delay it. A later cutoff logs a session notice, quits,
and requests normal immediate-child cleanup; queued output and final child
status can be omitted. It is not a hard deadline for process reaping.

The status counters are `L` (delivered child lines), `B` (their original UTF-8
payload bytes plus one normalized LF per line), and `E` (stderr lines). `B`
excludes the stderr prefix and generated logs, and is not an exact pipe-byte
count: original CRLF and an unterminated EOF line are normalized. Elapsed time
measures the model-observed run, including startup and output drain; it freezes
at the terminal event. F5 resets those counters and elapsed time for the new
run, while preserving the session deadline and log history.

Add `--stdin` before `-- COMMAND` to show a focused `TextInput` and feedback row
within the inline UI. The default is 15 rows; `--ui-height=3` keeps a compact
status/input/hint layout, leaving more space for terminal scrollback. Heights
from 3 through 65,535 are accepted, and the runtime clamps the rendered frame
to the actual terminal height. Controls keep their rows before the log viewer;
a one- or two-row viewer renders without a border. A terminal too short or
narrow for the controls still clips them. Enter queues the draft and an LF. Only accepted
input clears the editor and produces a sanitized `[stdin queued]` echo; that
echo reports queue admission, not child acknowledgment. Queue-full, closed-input
and oversized-line errors leave the draft visible with retry or correction instructions.
Paste uses the single-line editor: line breaks and tabs become spaces, and
other control characters are removed. While the child is running, `q` is text.
Ctrl-D requests EOF after accepted input drains; it preserves any unsubmitted
draft. A child that does not read can prevent that drain, so Ctrl-C remains
available. Without `--stdin`, `q` still quits and the child receives closed stdin.

The model keeps one `ProcessInput` handle and passes its clone to
`ProcessSubscription::stdin` on every subscription update, preserving the
subscription ID. Its queue admits at most 16 lines of up to 64 KiB of UTF-8 bytes
each, plus one line being written. Sending never waits for pipe capacity;
child exit, stop or I/O failure can discard accepted input. The draft itself is
not byte capped. A handle belongs to one process run; a future restart needs a
fresh handle.

Every command run also owns a `ProcessControl` handle. The model polls its PID
and interrupt status every 250 ms; rendering uses that copied state. Ctrl-C
requests SIGINT for the immediate child on supported Unix hosts. A second
distinct press less than two seconds later quits the TUI and stops the child;
key-repeat events do neither. At exactly two seconds the next press starts a
new request window. Unsupported hosts, a child that has not started, closed
control, pending requests and failed signals produce explicit feedback.
`SIGINT sent` means the OS accepted the signal operation, not that a handler
acknowledged it. A child that handles the signal can keep reading queued input,
and requesting EOF does not disable interrupts. On an exited child, Ctrl-C or
`q` quits immediately. Signals target the immediate child, not its descendants.

Without `--exit-when-child-exits`, press F5 after the terminal event to rerun the
same command and arguments. Restart also requires confirmed child cleanup;
closed control alone is insufficient while accepted output is still draining.
If cleanup was initially unconfirmed, polling continues after the terminal
error so F5 becomes available when the retained child is eventually reaped.
That confirmation does not rewrite the earlier error or start a new process.
The model preserves the log viewer and unsubmitted draft, emits one
`[process] RESTART` boundary, resets per-run counters and creates fresh control
and input handles. Each process event carries its run generation, so late
stdout, stderr or terminal events from the previous run cannot change the new
run. This is same-command restart; process-tree control is unsupported.

The runtime's shared subscription queue holds at most 256 messages, and a loop
iteration drains at most 64 before returning to terminal work. After a full
batch, the next input poll does not wait before checking queued output again. A full queue
applies backpressure to producers; cancellation interrupts blocked sends.
`ProcessSubscription` delivers complete UTF-8 lines up to 64 KiB of payload,
excluding LF or CRLF. Final unterminated lines arrive at EOF; a trailing CR at
EOF is payload. Each pipe reader has a bounded assembly buffer, an 8 KiB input
buffer and at most one unsent line. Arbitrary model messages and retained model
history have no byte limit imposed by this queue.

The example opts into `ProcessSubscription::partial_output(true)`. When a child
flushes an unterminated prompt, the reader emits a cumulative preview before
its next pipe read. The example renders it asynchronously in the feedback row,
prefixed with `[stdout]` or `[stderr]`. This works with both the default
viewer and compact three-row interactive chrome. The most recently updated
nonempty preview is shown; each stream retains its own pending text. Control
and input-error feedback takes priority. Previews are always sanitized, including
in raw/SGR-only log modes, and never create hyperlinks, log records or counter
increments. A completed line clears that stream's preview and enters scrollback
once; termination and restart clear both previews. Old-generation updates cannot
replace the new run's prompt.

Preview events contain the original cumulative valid UTF-8 prefix, so consumers
must replace their preview rather than append it. The reader holds an incomplete
UTF-8 scalar and one possible CRLF delimiter until the next read resolves them.
It preserves original terminal controls for the consumer to sanitize as a whole;
this avoids treating split escape fragments as independent trusted text. The
64 KiB line bound and shared queue backpressure still apply. Cumulative previews
can add copying and queue traffic for output written a few bytes at a time;
they do not provide byte-efficient or byte-transparent streaming. The default
subscription remains complete-line-only unless the option is enabled.

Oversized lines, invalid UTF-8 and read errors stop output forwarding and
request immediate-child termination even if the model queue is full. The final
error names the stream, failure and actual child cleanup outcome, and reports
incomplete output. Already admitted lines precede that error; no successful
exit event follows it. Earlier valid previews may already be visible; an error
does not retroactively retract displayed text. Arbitrary binary streaming is
unsupported.
Monitoring errors, timeout and cancellation also check termination and reaping.
`Killed` requires an accepted kill request and an observed reaped status
(SIGKILL on Unix). Failed or unconfirmed cleanup is an error. After an accepted
kill, foreground reaping waits at most 500 ms; rejected kills never lead to a
blocking foreground wait. When ownership remains safe, an unconfirmed child is
handed to a background reaper. Thread-start failure is reported; a later wait
failure is logged and leaves restart disabled. After a wait error, a target
without a retained process handle refuses further waits and signals for that
PID. Restart stays unavailable until a successful wait proves reaping. This bounds
foreground reaping, not OS process lifetime or the total output-drain duration.
Natural exit waits for captured output delivery; timeout/cancellation can
interrupt that drain, and a full canceled queue may prevent final-status
delivery. A descendant that keeps a captured pipe open can delay natural
completion indefinitely unless a timeout or cancellation interrupts the drain.
The example stops the immediate child. A descendant can also retain stdin and
keep an input worker blocked after cancellation; a blocked write may deliver a
prefix before it returns. Byte-transparent partial log output, descendant cleanup and the
remaining CLI/registry workflow are still unfinished agent-shell work.

Choose a log policy in the model's returned command:

```rust
use ftui::runtime::Cmd;

let plain: Cmd<()> = Cmd::log("untrusted output");
let colored: Cmd<()> = Cmd::log_sgr_only("\x1b[31merror: connection closed");
let trusted: Cmd<()> = Cmd::log_raw("\x1b[34mtrusted terminal output\x1b[0m");
```

`Cmd::log_raw` permits terminal commands and is only for trusted text.
`Cmd::log_text` accepts a policy-bearing `ftui::render::sanitize::Text`; the
writer still applies that policy at output. Native `Program` treats all four
constructors as logical-line commands: it filters first, normalizes LF/CRLF to
CRLF, and adds a final CRLF when missing. Raw commands therefore are not a
byte-transparent stream. Direct `TerminalWriter` methods and standalone
`OutMsg::Log { bytes, mode }` chunks do not append newlines.

The simulator, experimental `WasmRunner`, and web `StepProgram` capture filtered
logical strings without native CRLF formatting or style resets. Hosts consuming
those strings remain responsible for their output surface. The standalone
render worker carries the same explicit policy, but `Program` currently writes
directly through its own `TerminalWriter`.

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
- Inline mode: `.screen_mode(ScreenMode::Inline { ui_height: .. })` (or `App::inline(model, ui_height)`)
- Alt-screen: `.screen_mode(ScreenMode::AltScreen)` (or `App::fullscreen(model)`)

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
