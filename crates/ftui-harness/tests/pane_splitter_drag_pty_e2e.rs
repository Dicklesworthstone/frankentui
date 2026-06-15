#![forbid(unsafe_code)]
#![cfg(unix)]

//! PTY-level pane-splitter drag E2E (bd-x9lqw acceptance #3).
//!
//! Drives genuine SGR mouse sequences over a real PTY into the
//! `pane_splitter_pty_harness` binary (which runs the real terminal runtime and
//! the production `PaneTerminalAdapter -> operations_for_transition -> PaneTree`
//! path) and asserts that the live split ratio changes -- in **both** inline and
//! alt-screen modes. This is the one Definition-of-Done item the in-process
//! `ftui-runtime` adapter integration tests could not cover: a true terminal
//! round-trip that exercises crossterm SGR decoding, mouse capture, hit-testing,
//! rendering, and teardown.
//!
//! The harness applies transitions with a FIXED pressure profile, so the
//! reported ratio is a deterministic function of the pointer column and is
//! identical across screen modes (a vertical splitter's first-pane share depends
//! only on the horizontal geometry, which is the same width in both modes).

use std::time::Duration;

use ftui_core::terminal_session::SessionOptions;
use ftui_pty::{CleanupExpectations, PtyConfig, assert_terminal_restored, spawn_command};
use portable_pty::CommandBuilder;

const PTY_COLS: u16 = 80;
const PTY_ROWS: u16 = 24;

// SGR mouse coordinates are 1-based; crossterm reports them 0-based (col-1).
// The root 1:1 split over an 80-wide area puts the vertical splitter boundary at
// 0-based column 40 => SGR column 41. SGR row 3 => 0-based row 2.
//
// Button codes: 0 = left press/release, 32 = motion with left button held.
// Trailing `M` = press/motion, trailing `m` = release.

/// Press the splitter, drag rightward to the far side, release. Grows the first
/// (left) pane: final share well above 5000 bps.
const RIGHT_DRAG: &[u8] = b"\x1b[<0;41;3M\x1b[<32;56;3M\x1b[<32;65;3M\x1b[<0;65;3m";

/// Press the splitter, drag leftward to the near side, release. Shrinks the
/// first (left) pane: final share well below 5000 bps.
const LEFT_DRAG: &[u8] = b"\x1b[<0;41;3M\x1b[<32;26;3M\x1b[<32;17;3M\x1b[<0;17;3m";

#[derive(Debug)]
struct PaneResult {
    mode: String,
    initial_bps: u32,
    final_bps: u32,
    applied_ops: u64,
    down_resolved: bool,
    committed: bool,
    tree_valid: bool,
}

fn parse_marker(output: &[u8]) -> PaneResult {
    let text = String::from_utf8_lossy(output);
    let line = text
        .lines()
        .find(|line| line.contains("PANE_RESULT "))
        .unwrap_or_else(|| panic!("no PANE_RESULT marker in PTY output:\n{text}"));
    // Strip any leading restore escapes; the kv tail is clean ASCII.
    let kv = line
        .split_once("PANE_RESULT ")
        .expect("marker prefix present")
        .1;

    let mut mode = None;
    let mut initial_bps = None;
    let mut final_bps = None;
    let mut applied_ops = None;
    let mut down_resolved = None;
    let mut committed = None;
    let mut tree_valid = None;
    for pair in kv.split_whitespace() {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "mode" => mode = Some(value.to_string()),
            "initial_bps" => initial_bps = value.parse().ok(),
            "final_bps" => final_bps = value.parse().ok(),
            "applied_ops" => applied_ops = value.parse().ok(),
            "down_resolved" => down_resolved = Some(value == "true"),
            "committed" => committed = Some(value == "true"),
            "tree_valid" => tree_valid = Some(value == "true"),
            _ => {}
        }
    }

    PaneResult {
        mode: mode.expect("mode field"),
        initial_bps: initial_bps.expect("initial_bps field"),
        final_bps: final_bps.expect("final_bps field"),
        applied_ops: applied_ops.expect("applied_ops field"),
        down_resolved: down_resolved.expect("down_resolved field"),
        committed: committed.expect("committed field"),
        tree_valid: tree_valid.expect("tree_valid field"),
    }
}

/// Spawn the harness under a real PTY, drive a scripted SGR drag, and return the
/// full captured PTY output (raw bytes, including the result marker emitted after
/// the terminal is restored).
fn run_drag(mode: &str, sgr: &[u8]) -> Vec<u8> {
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_pane_splitter_pty_harness"));
    cmd.env("PANE_HARNESS_SCREEN_MODE", mode);
    cmd.env("PANE_HARNESS_UI_HEIGHT", "12");
    cmd.env("PANE_HARNESS_EXIT_AFTER_MS", "3000");

    let config = PtyConfig::default()
        .with_size(PTY_COLS, PTY_ROWS)
        .with_test_name(format!("pane_splitter_drag_{mode}"))
        .logging(false);

    let mut session = spawn_command(config, cmd).expect("spawn pane splitter PTY harness");

    // Let the runtime enter raw mode and render its first frame (so the pane area
    // is captured) before the scripted gesture arrives. PTY input is buffered, so
    // this is belt-and-suspenders rather than strictly required.
    std::thread::sleep(Duration::from_millis(350));
    session.send_input(sgr).expect("send SGR drag sequence");

    let status = session
        .wait_and_drain(Duration::from_secs(10))
        .expect("wait_and_drain harness");
    assert!(status.success(), "harness exited with failure: {status:?}");
    session.output().to_vec()
}

#[test]
fn pty_splitter_drag_right_grows_first_pane_in_both_modes() {
    for mode in ["alt", "inline"] {
        let output = run_drag(mode, RIGHT_DRAG);
        let result = parse_marker(&output);

        assert_eq!(result.mode, mode, "marker reported the wrong screen mode");
        assert!(
            result.down_resolved,
            "[{mode}] mouse-down did not grab the splitter (hit-test failed)"
        );
        assert!(
            result.applied_ops > 0,
            "[{mode}] no pane operations were applied by the drag"
        );
        assert!(result.committed, "[{mode}] drag never committed on release");
        assert!(result.tree_valid, "[{mode}] pane tree invalid after drag");
        assert_eq!(
            result.initial_bps, 5000,
            "[{mode}] expected an initial 50/50 split"
        );
        assert!(
            result.final_bps > result.initial_bps + 500,
            "[{mode}] rightward drag should grow the first pane: initial={} final={}",
            result.initial_bps,
            result.final_bps
        );
    }
}

#[test]
fn pty_splitter_drag_left_shrinks_first_pane_in_both_modes() {
    for mode in ["alt", "inline"] {
        let output = run_drag(mode, LEFT_DRAG);
        let result = parse_marker(&output);

        assert!(
            result.down_resolved,
            "[{mode}] mouse-down did not grab the splitter (hit-test failed)"
        );
        assert!(
            result.applied_ops > 0,
            "[{mode}] no pane operations were applied by the drag"
        );
        assert!(result.committed, "[{mode}] drag never committed on release");
        assert!(result.tree_valid, "[{mode}] pane tree invalid after drag");
        assert!(
            result.final_bps + 500 < result.initial_bps,
            "[{mode}] leftward drag should shrink the first pane: initial={} final={}",
            result.initial_bps,
            result.final_bps
        );
    }
}

#[test]
fn pty_splitter_drag_ratio_is_mode_independent_and_deterministic() {
    let alt_a = parse_marker(&run_drag("alt", RIGHT_DRAG));
    let alt_b = parse_marker(&run_drag("alt", RIGHT_DRAG));
    let inline = parse_marker(&run_drag("inline", RIGHT_DRAG));

    // Determinism: identical scripted input yields a byte-for-byte identical
    // final ratio across runs (fixed-pressure bridge, no wall-clock dependency).
    assert_eq!(
        alt_a.final_bps, alt_b.final_bps,
        "alt-screen splitter drag was not deterministic across runs"
    );

    // Mode independence: the same horizontal drag produces the same first-pane
    // share in inline and alt-screen modes (resize geometry is screen-mode
    // independent at the adapter layer).
    assert_eq!(
        alt_a.final_bps, inline.final_bps,
        "split ratio differs across screen modes: alt={} inline={}",
        alt_a.final_bps, inline.final_bps
    );
}

#[test]
fn pty_splitter_drag_restores_terminal_cleanly() {
    for (mode, alternate_screen) in [("alt", true), ("inline", false)] {
        let output = run_drag(mode, RIGHT_DRAG);

        // Conservative, capability-independent teardown expectations: the cursor
        // is always restored and alt-screen is always exited for alt mode. We do
        // not assert mouse/bracketed-paste/focus disable sequences here because
        // those features are sanitized against detected terminal capabilities.
        let options = SessionOptions {
            alternate_screen,
            mouse_capture: false,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
            intercept_signals: true,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert_terminal_restored(&output, &expectations)
            .unwrap_or_else(|err| panic!("[{mode}] terminal cleanup verification failed: {err}"));
    }
}
