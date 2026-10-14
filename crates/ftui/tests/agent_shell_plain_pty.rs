#![forbid(unsafe_code)]
#![cfg(unix)]

//! G48 (bd-g00-root-epic-ewths.47.4): the agent shell in a plain PTY, with no
//! multiplexer, delivers every log line to terminal history whichever inline
//! transport its DECSTBM self-test selects.
//!
//! The far end of the PTY is ftui-pty's `VirtualTerminal`: rows that scroll out
//! of a region starting at row 1 enter its scrollback, as in xterm. When the
//! driver answers CPR, the self-test passes and the scroll-region transport
//! runs; when it stays silent, the self-test times out after 150 ms and the
//! overlay transport runs. Both runs must leave lines 1..N in history exactly
//! once, in order, under one final UI frame.
//!
//! The PTY's child is this test binary, re-run with only
//! `agent_shell_child_entry` selected: a prebuilt example binary could be
//! stale for a `--test` build, and this one is always the checkout's code.

use std::time::{Duration, Instant};

use ftui::agent_shell::{CHILD_LINK_RETENTION_LINES, StreamingHarness, StreamingOptions};
use ftui::{App, ScreenMode};
use ftui_pty::virtual_terminal::VirtualTerminal;
use ftui_pty::{PtyConfig, spawn_command};
use portable_pty::CommandBuilder;

const CHILD_ARGUMENTS: &str = "FTUI_PLAIN_PTY_AGENT_SHELL_ARGS";
const COLS: u16 = 100;
const ROWS: u16 = 30;
/// agent_shell's default inline UI height.
const UI_HEIGHT: u16 = 3;
const LINES: usize = 2_000;
const CPR_QUERY: &[u8] = b"\x1b[6n";
/// The child prints this after the app returns; nothing follows it.
const CHILD_DONE: &str = "test result:";

/// The agent shell itself, when re-run by the tests below.
#[test]
#[ignore = "entry point: the plain-PTY tests run it as their PTY child"]
fn agent_shell_child_entry() {
    let arguments = std::env::var(CHILD_ARGUMENTS).expect("run only by the plain-PTY tests");
    let options =
        StreamingOptions::parse_agent_shell(arguments.split('\x1f').map(str::to_owned), None, None)
            .expect("valid agent_shell arguments");
    let ui_height = options.ui_height();
    App::new(StreamingHarness::new(options))
        .screen_mode(ScreenMode::Inline { ui_height })
        .with_hyperlink_limit(CHILD_LINK_RETENTION_LINES)
        .run()
        .expect("agent shell ran");
}

struct PlainPtyRun {
    output: Vec<u8>,
    terminal: VirtualTerminal,
    cpr_replies: usize,
}

/// Run `agent_shell --exit-when-child-exits -- seq 1 LINES` in a PTY that the
/// terminal model reads, answering every CPR query when `answer_cpr` is set.
fn run_plain_pty(answer_cpr: bool) -> PlainPtyRun {
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
    cmd.args([
        "--ignored",
        "--exact",
        "agent_shell_child_entry",
        "--nocapture",
        "--test-threads=1",
        "-q",
    ]);
    let arguments = [
        "--exit-when-child-exits",
        "--",
        "seq",
        "1",
        &LINES.to_string(),
    ];
    cmd.env(CHILD_ARGUMENTS, arguments.join("\x1f"));
    // A plain xterm: none of the identity variables capability detection
    // reads (a WezTerm mux pane, for one, is treated as a multiplexer), and
    // no forced transport or probe switch.
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
        "FTUI_SCROLL_REGION",
        "FTUI_SYNC_OUTPUT",
        "FTUI_CAPS_PROBE",
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
    .expect("spawn the agent shell in a PTY");

    let mut terminal = VirtualTerminal::new(COLS, ROWS);
    terminal.set_max_scrollback(LINES * 2);
    let mut fed = 0;
    let mut cpr_replies = 0;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let output = session.read_output();
        // Feed up to and including each query, so a reply reports the
        // cursor exactly where the query left it.
        while let Some(query) = find(&output[fed..], CPR_QUERY) {
            let end = fed + query + CPR_QUERY.len();
            terminal.feed(&output[fed..end]);
            fed = end;
            if answer_cpr {
                session
                    .send_input(&terminal.cpr_response())
                    .expect("answer CPR");
                cpr_replies += 1;
            }
        }
        terminal.feed(&output[fed..]);
        fed = output.len();
        if String::from_utf8_lossy(&output).contains(CHILD_DONE) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "agent shell did not finish; last output:\n{}",
            String::from_utf8_lossy(&output[output.len().saturating_sub(2_000)..])
        );
        // Well inside the self-test's 150 ms reply window.
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = session
        .wait_and_drain(Duration::from_secs(2))
        .expect("child exit");
    assert!(status.success(), "agent shell child failed: {status:?}");
    let output = session.read_output();
    terminal.feed(&output[fed..]);
    PlainPtyRun {
        output,
        terminal,
        cpr_replies,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The start of the child's output with controls escaped, for failures.
fn excerpt(output: &[u8]) -> String {
    output[..output.len().min(1_200)].escape_ascii().to_string()
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

/// Lines 1..=LINES each once and in order; one status row (the final frame).
fn assert_every_line_reached_history(run: &PlainPtyRun, transport: &str) {
    let rows = transcript(&run.terminal);
    let numbers: Vec<usize> = rows
        .iter()
        .filter_map(|row| row.trim().parse().ok())
        .filter(|number| (1..=LINES).contains(number))
        .collect();
    let expected: Vec<usize> = (1..=LINES).collect();
    let first_divergence = numbers
        .iter()
        .zip(&expected)
        .position(|(seen, want)| seen != want);
    assert!(
        numbers == expected,
        "{transport}: {} of {LINES} numbered rows, first divergence at {first_divergence:?}",
        numbers.len()
    );
    let status_rows = rows
        .iter()
        .filter(|row| row.contains("L:") && row.contains(" B:"))
        .count();
    assert_eq!(
        status_rows, 1,
        "{transport}: the UI must appear once, as its final frame"
    );
    assert!(
        rows.iter().any(|row| row.contains(&format!("L:{LINES} "))),
        "{transport}: the final frame must count every line"
    );
}

#[test]
fn plain_pty_answering_cpr_uses_the_scroll_region_and_keeps_every_line() {
    let run = run_plain_pty(true);
    let region = format!("\x1b[1;{}r", ROWS - UI_HEIGHT);
    assert!(
        run.cpr_replies >= 1,
        "the DECSTBM self-test asks for CPR; output began:\n{}",
        excerpt(&run.output)
    );
    // One region for the self-test, then the transport's own.
    assert!(
        count(&run.output, region.as_bytes()) >= 2,
        "an answered self-test must select the scroll-region transport; output began:\n{}",
        excerpt(&run.output)
    );
    assert_every_line_reached_history(&run, "scroll region");
}

#[test]
fn plain_pty_without_cpr_falls_back_to_overlay_and_keeps_every_line() {
    let run = run_plain_pty(false);
    let region = format!("\x1b[1;{}r", ROWS - UI_HEIGHT);
    assert!(
        count(&run.output, CPR_QUERY) >= 1,
        "the self-test still asks; nobody answers; output began:\n{}",
        excerpt(&run.output)
    );
    assert_eq!(
        count(&run.output, region.as_bytes()),
        1,
        "only the unanswered self-test may set the region; overlay never does"
    );
    assert_every_line_reached_history(&run, "overlay");
}
