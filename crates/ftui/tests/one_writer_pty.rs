#![forbid(unsafe_code)]
#![cfg(unix)]

//! G27 (bd-g00-root-epic-ewths.33.4): while the UI owns the terminal, nothing
//! but `TerminalWriter` writes to it.
//!
//! A task that panics is recovered by the runtime and the program keeps
//! drawing. The panic's report used to reach the terminal at once, twice: the
//! std panic hook wrote it to stderr (the same terminal), and the task
//! executor added an `eprintln!`. Both landed between frames, over the UI, in
//! raw mode. Now the report waits for the teardown and prints, once, after the
//! terminal is back in cooked mode.
//!
//! The probe also runs with `FTUI_DEBUG_TRACE=1` and records into a
//! `DiagnosticLog::with_stderr()` on every update; both used to write a line
//! to stderr per frame, and with stderr on the UI's terminal they now write
//! none.
//!
//! The PTY's child is this test binary, re-run with only
//! `panicking_task_child_entry` selected; its far end is ftui-pty's
//! `VirtualTerminal`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ftui::prelude::*;
use ftui::widgets::diagnostics::{DiagnosticLog, DiagnosticRecord};
use ftui::widgets::paragraph::Paragraph;
use ftui_pty::virtual_terminal::VirtualTerminal;
use ftui_pty::{PtyConfig, spawn_command};
use portable_pty::CommandBuilder;

const CHILD_MARKER: &str = "FTUI_ONE_WRITER_PTY_CHILD";
const COLS: u16 = 100;
const ROWS: u16 = 24;
const PANIC_TEXT: &str = "one-writer probe: this task panics on purpose";
/// Frames drawn after the task has unwound, before the program quits.
const FRAMES_AFTER_PANIC: u32 = 3;
/// The child prints this after the app returns; nothing follows it.
const CHILD_DONE: &str = "test result:";
/// Every diagnostic line the probe records carries this.
const DIAGNOSTIC_MARKER: &str = "one-writer-probe-diagnostic";

#[derive(Debug, Clone)]
struct ProbeDiagnostic {
    update: u32,
}

impl DiagnosticRecord for ProbeDiagnostic {
    fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"{DIAGNOSTIC_MARKER}","update":{}}}"#,
            self.update
        )
    }
}

/// Set while the panicking task unwinds, which is after the panic hook has
/// returned: a report the hook wrote at once is already in the output when
/// the model sees this.
static TASK_UNWOUND: AtomicBool = AtomicBool::new(false);

struct PanickingTask {
    frames_after_panic: u32,
    updates: u32,
    diagnostics: DiagnosticLog<ProbeDiagnostic>,
}

#[derive(Debug, Clone)]
enum Msg {
    Tick,
}

impl From<Event> for Msg {
    fn from(_: Event) -> Self {
        Msg::Tick
    }
}

impl Model for PanickingTask {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Msg> {
        Cmd::batch(vec![
            Cmd::task(|| {
                struct MarkUnwound;
                impl Drop for MarkUnwound {
                    fn drop(&mut self) {
                        TASK_UNWOUND.store(true, Ordering::SeqCst);
                    }
                }
                let _mark = MarkUnwound;
                panic!("{PANIC_TEXT}")
            }),
            Cmd::tick(Duration::from_millis(40)),
        ])
    }

    fn update(&mut self, _tick: Msg) -> Cmd<Msg> {
        self.updates += 1;
        self.diagnostics.record(ProbeDiagnostic {
            update: self.updates,
        });
        if TASK_UNWOUND.load(Ordering::SeqCst) {
            self.frames_after_panic += 1;
            if self.frames_after_panic > FRAMES_AFTER_PANIC {
                return Cmd::quit();
            }
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let text = if self.frames_after_panic == 0 {
            "probe: waiting for the task".to_owned()
        } else {
            format!("probe: frame {} after the panic", self.frames_after_panic)
        };
        Paragraph::new(text).render(Rect::new(0, 0, frame.width(), 1), frame);
    }
}

/// The probe program, when re-run by the test below.
#[test]
#[ignore = "entry point: the one-writer PTY test runs it as its PTY child"]
fn panicking_task_child_entry() {
    std::env::var(CHILD_MARKER).expect("run only by the one-writer PTY test");
    App::new(PanickingTask {
        frames_after_panic: 0,
        updates: 0,
        diagnostics: DiagnosticLog::new().with_stderr(),
    })
    .screen_mode(ScreenMode::Inline { ui_height: 2 })
    .run()
    .expect("probe ran");
}

/// Run the probe in a PTY and return everything it wrote.
fn run_probe() -> Vec<u8> {
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
    cmd.args([
        "--ignored",
        "--exact",
        "panicking_task_child_entry",
        "--nocapture",
        "--test-threads=1",
        "-q",
    ]);
    cmd.env(CHILD_MARKER, "1");
    cmd.env("FTUI_DEBUG_TRACE", "1");
    // A plain xterm, whatever terminal runs the test.
    for name in [
        "TMUX",
        "TMUX_PANE",
        "STY",
        "ZELLIJ",
        "ZELLIJ_SESSION_NAME",
        "WEZTERM_UNIX_SOCKET",
        "WEZTERM_PANE",
        "WEZTERM_EXECUTABLE",
        "KITTY_WINDOW_ID",
        "WT_SESSION",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "LC_TERMINAL",
        "COLORTERM",
        "RUST_BACKTRACE",
    ] {
        cmd.env_remove(name);
    }
    let mut session = spawn_command(
        PtyConfig::default()
            .with_size(COLS, ROWS)
            .with_term("xterm-256color")
            .logging(false),
        cmd,
    )
    .expect("spawn the probe in a PTY");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let output = session.read_output();
        if String::from_utf8_lossy(&output).contains(CHILD_DONE) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "probe did not finish; last output:\n{}",
            String::from_utf8_lossy(&output[output.len().saturating_sub(2_000)..])
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = session
        .wait_and_drain(Duration::from_secs(2))
        .expect("child exit");
    assert!(status.success(), "probe child failed: {status:?}");
    session.read_output()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// History then screen, top to bottom.
fn transcript(terminal: &VirtualTerminal) -> Vec<String> {
    (0..terminal.scrollback_len())
        .filter_map(|index| terminal.scrollback_line(index))
        .chain((0..ROWS).map(|row| terminal.row_text(row)))
        .collect()
}

#[test]
fn a_recovered_task_panic_is_reported_after_the_teardown_not_over_the_ui() {
    let output = run_probe();

    assert_eq!(
        count(&output, b"[FTUI "),
        0,
        "FTUI_DEBUG_TRACE wrote to stderr, the UI's terminal; output:\n{}",
        output.escape_ascii()
    );
    assert_eq!(
        count(&output, DIAGNOSTIC_MARKER.as_bytes()),
        0,
        "DiagnosticLog::with_stderr wrote to the UI's terminal; output:\n{}",
        output.escape_ascii()
    );
    assert_eq!(
        count(&output, PANIC_TEXT.as_bytes()),
        1,
        "the panic must be reported exactly once; output:\n{}",
        output.escape_ascii()
    );
    let report_at = find(&output, PANIC_TEXT.as_bytes()).expect("counted above");

    // Everything before the report: the UI already shows frames drawn after
    // the task unwound, so the report was not written when the task panicked.
    let mut before_report = VirtualTerminal::new(COLS, ROWS);
    before_report.feed(&output[..report_at]);
    let screen = before_report.screen_text();
    assert!(
        screen.contains("after the panic") && !screen.contains("waiting for the task"),
        "the report reached the terminal while the UI was still drawing; screen before it:\n{screen}"
    );

    // The whole run: the report sits on rows of its own, starting at column
    // 0 (cooked mode is back, so its newlines return the carriage), with the
    // panic's location.
    let mut terminal = VirtualTerminal::new(COLS, ROWS);
    terminal.feed(&output);
    let rows = transcript(&terminal);
    assert!(
        rows.iter().any(|row| row.trim_end() == PANIC_TEXT),
        "the panic message must start a row of its own; rows:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().any(|row| row.starts_with("thread '")
            && row.contains("panicked at")
            && row.contains("one_writer_pty.rs:")),
        "the report keeps the std hook's wording and location; rows:\n{}",
        rows.join("\n")
    );
}
