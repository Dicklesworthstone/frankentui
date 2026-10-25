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

use ftui::Model;
use ftui::agent_shell::{CHILD_LINK_RETENTION_LINES, Msg, StreamingHarness, StreamingOptions};
use ftui::{App, Event, KeyCode, KeyEvent, ScreenMode};
use ftui_pty::virtual_terminal::VirtualTerminal;
use ftui_pty::{PtyConfig, spawn_command};
use portable_pty::{CommandBuilder, PtySize};

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
        .chain((0..terminal.height()).map(|row| terminal.row_text(row)))
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
    // Plain xterm-256color is not on the synchronized-output allowlist and
    // the model answers no DECRQM, so this is the Hybrid strategy: DECSTBM
    // without DEC 2026 brackets.
    assert_eq!(
        count(&run.output, b"\x1b[?2026h"),
        0,
        "plain xterm runs the Hybrid strategy, without synchronized output"
    );
    assert_every_line_reached_history(&run, "hybrid scroll region");
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

const JOURNEY_UI_HEIGHT: u16 = 5;
const JOURNEY_LINES: usize = 1_000;
const JOURNEY_MAX_BYTES_PER_LINE: usize = 400;
const JOURNEY_STORM_SIZES: [(u16, u16); 5] = [(100, 36), (92, 24), (110, 30), (96, 34), (104, 26)];

fn journey_expected_visible(kind: usize) -> String {
    let raw = match kind {
        0 => "plain ascii payload",
        1 => "wide 漢字かなカナ한글 payload",
        2 => "emoji 👩\u{200d}🔬 🏳️\u{200d}🌈 👍🏽 payload",
        3 => "sgr bold red blue payload",
        4 => "osc52-clear-region",
        5 => "link https://example.invalid/x and controls",
        _ => unreachable!(),
    };
    let mut vt = VirtualTerminal::new(COLS, 1);
    vt.feed_str(raw);
    vt.row_text(0)
}

fn journey_child_script(marker: &std::path::Path, pid_file: &std::path::Path) -> String {
    let half = JOURNEY_LINES / 2;
    let m = marker.display();
    let p = pid_file.display();
    format!(
        "if [ -e '{m}' ]; then echo second-run; exit 0; fi; : > '{m}'; \
         sleep 60 & echo $! > '{p}'; \
         line() {{ case $(($1 % 6)) in \
           0) printf 'M %d plain ascii payload\\n' \"$1\";; \
           1) printf 'M %d wide \\346\\274\\242\\345\\255\\227\\343\\201\\213\\343\\201\\252\\343\\202\\253\\343\\203\\212\\355\\225\\234\\352\\270\\200 payload\\n' \"$1\";; \
           2) printf 'M %d emoji \\360\\237\\221\\251\\342\\200\\215\\360\\237\\224\\254 \\360\\237\\217\\263\\357\\270\\217\\342\\200\\215\\360\\237\\214\\210 \\360\\237\\221\\215\\360\\237\\217\\275 payload\\n' \"$1\";; \
           3) printf 'M %d \\033[1;31msgr bold red\\033[0m \\033[38;5;33mblue\\033[0m payload\\n' \"$1\";; \
           4) printf 'M %d osc52\\033]52;c;cHdu\\007-clear\\033[2J\\033[1;1H-region\\033[1;3r\\033]2;pwn\\007\\n' \"$1\";; \
           5) printf 'M %d \\033]8;;https://example.invalid/\\033\\\\link\\033]8;;\\033\\\\ https://example.invalid/x and \\007\\010\\016\\033ccontrols\\n' \"$1\";; \
         esac; }}; \
         i=1; while [ $i -le {half} ]; do line $i; i=$((i+1)); done; \
         printf 'type now: '; IFS= read -r l; echo \"got:$l\"; \
         while [ $i -le {JOURNEY_LINES} ]; do line $i; i=$((i+1)); done; \
         trap 'echo interrupted; exit 130' INT; echo waiting; \
         while :; do sleep 0.1; done"
    )
}

fn feed_and_answer_cpr(
    session: &mut ftui_pty::PtySession,
    terminal: &mut VirtualTerminal,
    fed: &mut usize,
    answer_cpr: bool,
    cpr_replies: &mut usize,
) -> Vec<u8> {
    let output = session.read_output();
    while let Some(query) = find(&output[*fed..], CPR_QUERY) {
        let end = *fed + query + CPR_QUERY.len();
        terminal.feed(&output[*fed..end]);
        *fed = end;
        if answer_cpr {
            session
                .send_input(&terminal.cpr_response())
                .expect("answer CPR");
            *cpr_replies += 1;
        }
    }
    terminal.feed(&output[*fed..]);
    *fed = output.len();
    output
}

fn wait_for_screen_text(
    session: &mut ftui_pty::PtySession,
    terminal: &mut VirtualTerminal,
    fed: &mut usize,
    answer_cpr: bool,
    cpr_replies: &mut usize,
    needle: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let output = feed_and_answer_cpr(session, terminal, fed, answer_cpr, cpr_replies);
        let all_rows = transcript(terminal);
        if all_rows.iter().any(|row| row.contains(needle))
            || String::from_utf8_lossy(&output).contains(needle)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; screen:\n{}",
            terminal.screen_text()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn run_plain_pty_flagship_journey(answer_cpr: bool) {
    let suffix = format!(
        "{}_{}_{}",
        std::process::id(),
        if answer_cpr { "cpr" } else { "nocpr" },
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let marker = std::env::temp_dir().join(format!("ftui_plain_pty_restarted_{suffix}"));
    let pid_file = std::env::temp_dir().join(format!("ftui_plain_pty_bg_{suffix}.pid"));
    let child_script = journey_child_script(&marker, &pid_file);

    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
    cmd.args([
        "--ignored",
        "--exact",
        "agent_shell_child_entry",
        "--nocapture",
        "--test-threads=1",
        "-q",
    ]);
    let ui_arg = format!("--ui-height={JOURNEY_UI_HEIGHT}");
    let arguments = ["--pty", &ui_arg, "--", "sh", "-c", &child_script];
    cmd.env(CHILD_ARGUMENTS, arguments.join("\x1f"));
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
    .expect("spawn agent_shell journey in PTY");

    let mut terminal = VirtualTerminal::new(COLS, ROWS);
    terminal.set_max_scrollback(JOURNEY_LINES * 4);
    let mut fed = 0;
    let mut cpr_replies = 0;

    // 1. Wait for the child to stream the first half and prompt on stdin.
    wait_for_screen_text(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
        "type now",
        Duration::from_secs(60),
    );

    // 2. Type "hello" character by character, timing PTY keystroke-to-echo latency, then Enter.
    let mut echo_latencies_ms = Vec::new();
    for ch in "hello".chars() {
        let before_len = session.read_output().len();
        let sent_at = Instant::now();
        session
            .send_input(&[ch as u8])
            .expect("send keystroke to agent_shell");
        let echo_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let output = feed_and_answer_cpr(
                &mut session,
                &mut terminal,
                &mut fed,
                answer_cpr,
                &mut cpr_replies,
            );
            if output.len() > before_len && terminal.screen_text().contains(ch) {
                echo_latencies_ms.push(sent_at.elapsed().as_secs_f64() * 1000.0);
                break;
            }
            assert!(
                Instant::now() < echo_deadline,
                "keystroke {ch:?} was not echoed within 2s"
            );
            std::thread::sleep(Duration::from_micros(250));
        }
    }
    session.send_input(b"\r").expect("send Enter");
    wait_for_screen_text(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
        "[stdin queued] hello",
        Duration::from_secs(10),
    );

    // 3. Drive a 50-resize storm while the second half streams, draining PTY
    // output before each resize just as a real terminal emulator does.
    for step in 0..50 {
        let _ = feed_and_answer_cpr(
            &mut session,
            &mut terminal,
            &mut fed,
            answer_cpr,
            &mut cpr_replies,
        );
        let (w, h) = JOURNEY_STORM_SIZES[step % JOURNEY_STORM_SIZES.len()];
        session
            .master()
            .resize(PtySize {
                rows: h,
                cols: w,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize PTY master");
        terminal.resize(w, h);
        for _ in 0..15 {
            let _ = feed_and_answer_cpr(
                &mut session,
                &mut terminal,
                &mut fed,
                answer_cpr,
                &mut cpr_replies,
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let _ = feed_and_answer_cpr(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
    );
    session
        .master()
        .resize(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("restore PTY size");
    terminal.resize(COLS, ROWS);

    // 4. Wait for child to finish streaming and enter its SIGINT wait loop.
    wait_for_screen_text(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
        "waiting",
        Duration::from_secs(60),
    );

    // 5. Send Ctrl-C to interrupt the child and wait for EXIT 130.
    session.send_input(b"\x03").expect("send Ctrl-C");
    wait_for_screen_text(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
        "EXIT 130",
        Duration::from_secs(30),
    );

    // 6. Send F5 to restart the child (polling until reaping completes) and wait for EXIT 0.
    let restart_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        session.send_input(b"\x1b[15~").expect("send F5");
        let step_deadline = Instant::now() + Duration::from_millis(600);
        let mut saw_second_run = false;
        while Instant::now() < step_deadline {
            let _ = feed_and_answer_cpr(
                &mut session,
                &mut terminal,
                &mut fed,
                answer_cpr,
                &mut cpr_replies,
            );
            if transcript(&terminal)
                .iter()
                .any(|row| row.contains("second-run"))
            {
                saw_second_run = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if saw_second_run {
            break;
        }
        assert!(
            Instant::now() < restart_deadline,
            "F5 never restarted the child; screen:\n{}",
            terminal.screen_text()
        );
    }
    wait_for_screen_text(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
        "EXIT 0",
        Duration::from_secs(30),
    );

    // 7. Quit agent_shell with 'q' and wait for test harness completion.
    session.send_input(b"q").expect("send q");
    wait_for_screen_text(
        &mut session,
        &mut terminal,
        &mut fed,
        answer_cpr,
        &mut cpr_replies,
        CHILD_DONE,
        Duration::from_secs(30),
    );
    let status = session
        .wait_and_drain(Duration::from_secs(2))
        .expect("child exit");
    assert!(
        status.success(),
        "agent_shell journey exited non-zero: {status:?}"
    );
    let output = session.read_output();
    terminal.feed(&output[fed..]);

    // 8. Verify every journey invariant.
    let expected_visible: [String; 6] = std::array::from_fn(journey_expected_visible);
    let rows = transcript(&terminal);
    let mut seen_numbers = Vec::new();
    for row in &rows {
        let trimmed = row.trim_end();
        if let Some(rest) = trimmed.strip_prefix("M ")
            && let Some((num_str, payload)) = rest.split_once(' ')
            && let Ok(num) = num_str.parse::<usize>()
            && (1..=JOURNEY_LINES).contains(&num)
        {
            let expect = &expected_visible[num % 6];
            if payload.starts_with(expect.as_str()) {
                seen_numbers.push(num);
            }
        }
    }
    seen_numbers.dedup();
    let expected: Vec<usize> = (1..=JOURNEY_LINES).collect();
    assert_eq!(
        seen_numbers, expected,
        "every hostile mixed line 1..={JOURNEY_LINES} must reach history in order"
    );

    let has_line = |needle: &str| rows.iter().any(|row| row.contains(needle));
    assert!(
        has_line("type now: got:hello"),
        "typed line must reach child"
    );
    assert!(
        has_line("[stdin queued] hello"),
        "typed line must be echoed to log"
    );
    assert!(
        has_line("interrupted") && has_line("[process] EXIT 130"),
        "Ctrl-C interrupt must be logged"
    );
    assert!(
        has_line("[process] RESTART run=")
            && has_line("second-run")
            && has_line("[process] EXIT 0"),
        "F5 restart and second run must be logged"
    );

    // Hostile injections must remain inert.
    assert_ne!(
        terminal.title(),
        "pwn",
        "OSC 2 title injection must be stripped"
    );
    assert!(
        !terminal.is_alternate_screen(),
        "primary screen must be active after exit"
    );
    assert!(
        terminal.cursor_visible(),
        "cursor must be visible after exit"
    );
    assert!(
        !terminal.origin_mode(),
        "origin mode must be off after exit"
    );
    assert!(
        !terminal.insert_mode(),
        "insert mode must be off after exit"
    );
    assert!(terminal.autowrap(), "autowrap must remain on after exit");
    assert_eq!(
        terminal.scroll_region(),
        (0, ROWS - 1),
        "DECSTBM scroll region must be reset after exit"
    );

    // Background process spawned by child's first run must have been reaped.
    let bg_pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("read background pid file")
        .trim()
        .parse()
        .expect("valid pid");
    let mut reaped = false;
    for _ in 0..20 {
        let alive = std::process::Command::new("kill")
            .args(["-0", &bg_pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(reaped, "background child process {bg_pid} must be reaped");

    // Bytes per line must stay O(1) bounded.
    let bytes_per_line = output.len() / JOURNEY_LINES;
    assert!(
        bytes_per_line <= JOURNEY_MAX_BYTES_PER_LINE,
        "bytes per line {bytes_per_line} exceeded bound {JOURNEY_MAX_BYTES_PER_LINE}"
    );

    // Keystroke-to-echo latency over PTY must stay under 50 ms.
    echo_latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p99_ms = echo_latencies_ms[echo_latencies_ms.len() - 1];
    assert!(
        p99_ms <= 50.0,
        "PTY keystroke-to-echo latency {p99_ms:.2} ms exceeded 50 ms bound"
    );
}

#[test]
fn plain_pty_flagship_journey_with_cpr() {
    run_plain_pty_flagship_journey(true);
}

#[test]
fn plain_pty_flagship_journey_without_cpr() {
    run_plain_pty_flagship_journey(false);
}

#[test]
fn agent_shell_keystroke_dispatch_budget_under_100us() {
    let options =
        StreamingOptions::parse_agent_shell(["--", "true"].map(str::to_owned), None, None)
            .expect("valid options");
    let mut harness = StreamingHarness::new(options);

    // Warm up allocator and caches before timing.
    for i in 0..64u32 {
        let ch = char::from_u32(b'a' as u32 + (i % 26)).unwrap();
        let _ = harness.update(Msg::from(Event::Key(KeyEvent::new(KeyCode::Char(ch)))));
    }
    let _ = harness.update(Msg::from(Event::Key(KeyEvent::new(KeyCode::Enter))));

    // Measure 200 keystroke dispatches in-process.
    let mut samples_us = Vec::with_capacity(200);
    for i in 0..200u32 {
        let ch = char::from_u32(b'a' as u32 + (i % 26)).unwrap();
        let ev = Event::Key(KeyEvent::new(KeyCode::Char(ch)));
        let start = Instant::now();
        let _ = harness.update(Msg::from(ev));
        samples_us.push(start.elapsed().as_secs_f64() * 1_000_000.0);
        if i % 32 == 31 {
            let _ = harness.update(Msg::from(Event::Key(KeyEvent::new(KeyCode::Backspace))));
        }
    }
    samples_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_us = samples_us[samples_us.len() / 2];
    let p99_us = samples_us[(samples_us.len() * 99) / 100];
    assert!(
        p50_us < 100.0,
        "in-process keystroke dispatch p50 {p50_us:.2} us (p99 {p99_us:.2} us) exceeded 100 us budget"
    );
    // In unoptimized debug builds, allow for OS scheduler preemption on p99;
    // in release builds, enforce p99 < 100 us strictly.
    let p99_limit_us = if cfg!(debug_assertions) { 500.0 } else { 100.0 };
    assert!(
        p99_us < p99_limit_us,
        "in-process keystroke dispatch p99 {p99_us:.2} us (p50 {p50_us:.2} us) exceeded {p99_limit_us:.0} us budget"
    );
}
