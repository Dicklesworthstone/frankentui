#![forbid(unsafe_code)]
#![cfg(unix)]

//! PTY-level pane interaction E2E suite (bd-a46q1.3).
//!
//! Companion to `pane_splitter_drag_pty_e2e.rs` (which proves pointer-drag
//! resize). This suite drives the *other* terminal-input modalities the
//! production `PaneTerminalAdapter` supports over a real PTY into the shared
//! `pane_splitter_pty_harness` binary, and asserts the live pane tree responds
//! correctly:
//!
//! - **Keyboard resize**: arrow keys / `+` / `-`, with `Shift` = 5x step. The
//!   harness resolves the splitter target from the rendered handles and routes
//!   the key through the documented `translate(event, target_hint)` contract — the
//!   exact path a focus-aware host uses. Nudges step by a fixed
//!   `PANE_SNAP_DEFAULT_STEP_BPS` (500 bps) per unit and are geometry-independent,
//!   so the reported ratios are *exact* and identical across screen modes.
//! - **Wheel nudge**: SGR scroll over the splitter handle (+/- 500 bps/line).
//! - **Escape recovery**: an armed pointer interaction is cleanly canceled.
//! - **Vertical axis**: up/down keyboard resize on a vertical root split.
//! - **Structural ops** (split/close/swap) over a real PTY via harness affordance
//!   keys, proving the operation -> render -> teardown stack. (The *input
//!   binding* for these in the production terminal adapter is bd-21pbi.2 scope;
//!   the harness exercises the operations directly so the PTY render/teardown path
//!   is still covered end-to-end.)
//! - **Capability matrix**: the canonical keyboard resize under several `TERM`
//!   profiles (xterm / screen / tmux), with clean teardown each time.
//!
//! ## Production keyboard bindings (bd-8e1oc)
//!
//! A second group (`pty_keymap_*`) drives the **production** terminal keyboard
//! binding (`ftui_runtime::pane_keymap`) over a real PTY via
//! `PANE_HARNESS_INPUT=keymap` — key -> `PaneCommand` -> resolve -> live tree,
//! NOT the affordance keys. Covered: keyboard focus navigation (Tab, Ctrl+Arrow),
//! split (Alt+s), close (Alt+w), and maximize (Alt+z), asserted via the marker's
//! `active_pane` / `node_count` / `maximized` fields, plus a TERM capability
//! matrix. (Resize-interrupt and focus-loss cancel paths remain covered
//! in-process by the `ftui-runtime` adapter tests because `ftui-pty` cannot
//! inject a mid-run `SIGWINCH` / focus event.)
//!
//! Starting state is always a 1:1 root split (5000 bps first-pane share).

use std::time::{Duration, Instant};

use ftui_core::terminal_session::SessionOptions;
use ftui_pty::{CleanupExpectations, PtyConfig, assert_terminal_restored, spawn_command};
use portable_pty::CommandBuilder;

const PTY_COLS: u16 = 80;
const PTY_ROWS: u16 = 24;

const STEP_BPS: u32 = 500; // PANE_SNAP_DEFAULT_STEP_BPS
const INITIAL_BPS: u32 = 5000;

// --- Terminal input sequences ---------------------------------------------
//
// Arrow keys (CSI). `Shift` is CSI parameter `1;2`.
const KEY_RIGHT: &[u8] = b"\x1b[C";
const KEY_LEFT: &[u8] = b"\x1b[D";
const KEY_UP: &[u8] = b"\x1b[A";
const KEY_DOWN: &[u8] = b"\x1b[B";
const KEY_SHIFT_RIGHT: &[u8] = b"\x1b[1;2C";
const KEY_SHIFT_LEFT: &[u8] = b"\x1b[1;2D";
// Quit (the harness maps `q` Press to a prompt-free shutdown).
const KEY_QUIT: &[u8] = b"q";
// Lone ESC, delivered on its own so crossterm decodes it as Esc (not a prefix).
const KEY_ESC: &[u8] = b"\x1b";

// SGR mouse: button 0 = left, 64 = scroll up, 65 = scroll down; trailing `M` =
// press/scroll, `m` = release. The 1:1 split over an 80-wide area puts the
// vertical splitter boundary at 0-based column 40 => SGR column 41; SGR row 3.
const MOUSE_DOWN_ON_SPLITTER: &[u8] = b"\x1b[<0;41;3M";
const MOUSE_DRAG_SPLITTER_RIGHT: &[u8] = b"\x1b[<32;61;3M";
const STALE_DRAG_AND_RELEASE: &[u8] = b"\x1b[<32;71;3M\x1b[<0;71;3m";
const SCROLL_UP_ON_SPLITTER: &[u8] = b"\x1b[<64;41;3M";
const SCROLL_DOWN_ON_SPLITTER: &[u8] = b"\x1b[<65;41;3M";

// Structural affordance keys (adapter-mode harness affordances).
const KEY_SPLIT: &[u8] = b"s";
const KEY_CLOSE: &[u8] = b"c";
const KEY_SWAP: &[u8] = b"w";

// --- Production keyboard bindings (PANE_HARNESS_INPUT=keymap, bd-8e1oc) ------
//
// Terminal keymap from `ftui_runtime::pane_keymap`. Tab/BackTab and CSI arrows
// (modifier param `1;5` = Ctrl) are robust over a PTY; Alt bindings are ESC-
// prefixed and must arrive as a single chunk so the parser decodes `Alt+<char>`.
const KEY_TAB: &[u8] = b"\x09"; // FocusNext
const KEY_CTRL_RIGHT: &[u8] = b"\x1b[1;5C"; // FocusDirectional(Right)
const KEY_ALT_S: &[u8] = b"\x1bs"; // Split(Horizontal)
const KEY_ALT_W: &[u8] = b"\x1bw"; // Close
const KEY_ALT_Z: &[u8] = b"\x1bz"; // Maximize

#[derive(Debug)]
struct PaneResult {
    mode: String,
    initial_bps: u32,
    final_bps: u32,
    applied_ops: u64,
    down_resolved: bool,
    committed: bool,
    tree_valid: bool,
    node_count: usize,
    first_leaf: String,
    canceled: bool,
    timed_out: bool,
    cancel_preserved_state: bool,
    post_cancel_events: u32,
    post_cancel_preserved_state: bool,
    history_entries: usize,
    history_cursor: usize,
    cancel_history_undo_redo: bool,
    dragging_seen: bool,
    termios_restored: bool,
    recovered_bytes: usize,
    /// Focused leaf surface key from the production keyboard controller.
    active_pane: String,
    /// Whether a pane is maximized (keymap mode).
    maximized: bool,
}

fn parse_marker(output: &[u8]) -> PaneResult {
    let text = String::from_utf8_lossy(output);
    let line = text
        .lines()
        .find(|line| line.contains("PANE_RESULT "))
        .unwrap_or_else(|| panic!("no PANE_RESULT marker in PTY output:\n{text}"));
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
    let mut node_count = None;
    let mut first_leaf = None;
    let mut canceled = None;
    let mut timed_out = None;
    let mut cancel_preserved_state = None;
    let mut post_cancel_events = None;
    let mut post_cancel_preserved_state = None;
    let mut history_entries = None;
    let mut history_cursor = None;
    let mut cancel_history_undo_redo = None;
    let mut dragging_seen = None;
    let mut termios_restored = None;
    let mut recovered_bytes = None;
    let mut active_pane = None;
    let mut maximized = None;
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
            "node_count" => node_count = value.parse().ok(),
            "first_leaf" => first_leaf = Some(value.to_string()),
            "canceled" => canceled = Some(value == "true"),
            "timed_out" => timed_out = Some(value == "true"),
            "cancel_preserved_state" => cancel_preserved_state = Some(value == "true"),
            "post_cancel_events" => post_cancel_events = value.parse().ok(),
            "post_cancel_preserved_state" => post_cancel_preserved_state = Some(value == "true"),
            "history_entries" => history_entries = value.parse().ok(),
            "history_cursor" => history_cursor = value.parse().ok(),
            "cancel_history_undo_redo" => cancel_history_undo_redo = Some(value == "true"),
            "dragging_seen" => dragging_seen = Some(value == "true"),
            "termios_restored" => termios_restored = Some(value == "true"),
            "recovered_bytes" => recovered_bytes = value.parse().ok(),
            "active_pane" => active_pane = Some(value.to_string()),
            "maximized" => maximized = Some(value == "true"),
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
        node_count: node_count.expect("node_count field"),
        first_leaf: first_leaf.expect("first_leaf field"),
        canceled: canceled.expect("canceled field"),
        timed_out: timed_out.expect("timed_out field"),
        cancel_preserved_state: cancel_preserved_state.expect("cancel_preserved_state field"),
        post_cancel_events: post_cancel_events.expect("post_cancel_events field"),
        post_cancel_preserved_state: post_cancel_preserved_state
            .expect("post_cancel_preserved_state field"),
        history_entries: history_entries.expect("history_entries field"),
        history_cursor: history_cursor.expect("history_cursor field"),
        cancel_history_undo_redo: cancel_history_undo_redo.expect("cancel_history_undo_redo field"),
        dragging_seen: dragging_seen.expect("dragging_seen field"),
        termios_restored: termios_restored.expect("termios_restored field"),
        recovered_bytes: recovered_bytes.expect("recovered_bytes field"),
        active_pane: active_pane.expect("active_pane field"),
        maximized: maximized.expect("maximized field"),
    }
}

/// Inspect actual DEC private-mode transitions, including combined parameters.
/// Initial defensive resets cannot satisfy teardown after a later enable.
fn mouse_reporting_restored(output: &[u8]) -> Result<String, String> {
    let mut modes =
        [9_u16, 1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016].map(|mode| (mode, None, None));
    for (offset, prefix) in output.windows(3).enumerate() {
        if prefix != b"\x1b[?" {
            continue;
        }
        let tail = &output[offset + 3..];
        let Some(end) = tail.iter().position(|byte| (0x40..=0x7e).contains(byte)) else {
            continue;
        };
        let enabled = match tail[end] {
            b'h' => true,
            b'l' => false,
            _ => continue,
        };
        let Ok(parameters) = std::str::from_utf8(&tail[..end]) else {
            continue;
        };
        let Ok(parameters) = parameters
            .split(';')
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>()
        else {
            continue;
        };
        for (mode, last_enable, last_disable) in &mut modes {
            if parameters.contains(mode) {
                if enabled {
                    *last_enable = Some(offset);
                } else {
                    *last_disable = Some(offset);
                }
            }
        }
    }
    let evidence = modes
        .iter()
        .filter(|(_, enable, _)| enable.is_some())
        .map(|(mode, enable, disable)| format!("mode={mode},enable={enable:?},disable={disable:?}"))
        .collect::<Vec<_>>()
        .join(" ");
    for (mode, enable, disable) in modes {
        if [1000, 1002, 1006].contains(&mode) && enable.is_none() {
            return Err(format!(
                "required mouse mode {mode} was never enabled: {evidence}"
            ));
        }
        if let Some(enable) = enable
            && disable.is_none_or(|disable| disable < enable)
        {
            return Err(format!("mouse mode {mode} remains enabled: {evidence}"));
        }
    }
    Ok(evidence)
}

#[test]
fn mouse_cleanup_evaluator_requires_disable_after_every_enabled_mode() {
    let combined = b"\x1b[?1000;1002;1006h\x1b[?1000;1002;1006l";
    let separate = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1006l\x1b[?1002l\x1b[?1000l";
    assert!(mouse_reporting_restored(combined).is_ok());
    assert!(mouse_reporting_restored(separate).is_ok());
    for incomplete in [
        b"".as_slice(),
        b"\x1b[?1000;1002;1006l\x1b[?1000;1002;1006h",
        b"\x1b[?1000;1002;1006h\x1b[?1000;1002l",
        b"\x1b[?1000;1002;1006h\x1b[?1000;1002;1006l\x1b[?1002h",
        b"\x1b[?1000;1002;1003;1006h\x1b[?1000;1002;1006l",
    ] {
        assert!(
            mouse_reporting_restored(incomplete).is_err(),
            "incomplete teardown accepted: {incomplete:?}"
        );
    }
}

struct Scenario {
    mode: &'static str,
    axis: &'static str,
    term: Option<&'static str>,
    /// Harness input mode: `adapter` (default) or `keymap` (production keyboard).
    input: &'static str,
    /// Input chunks delivered sequentially with a small inter-chunk gap so that
    /// (for example) a lone trailing ESC is decoded on its own.
    parts: Vec<&'static [u8]>,
    exit_after_ms: u32,
    during_probe: bool,
}

impl Scenario {
    fn new(mode: &'static str, parts: Vec<&'static [u8]>) -> Self {
        Self {
            mode,
            axis: "horizontal",
            term: None,
            input: "adapter",
            parts,
            exit_after_ms: 3000,
            during_probe: false,
        }
    }

    fn axis(mut self, axis: &'static str) -> Self {
        self.axis = axis;
        self
    }

    /// Drive the production keyboard binding (`PANE_HARNESS_INPUT=keymap`).
    fn keymap(mut self) -> Self {
        self.input = "keymap";
        self
    }

    fn term(mut self, term: &'static str) -> Self {
        self.term = Some(term);
        self
    }

    fn exit_after_ms(mut self, ms: u32) -> Self {
        self.exit_after_ms = ms;
        self
    }

    fn during_probe(mut self) -> Self {
        self.during_probe = true;
        self
    }
}

/// Spawn the harness under a real PTY, deliver the scripted input chunks, and
/// return the full captured PTY output (including the marker emitted after the
/// terminal is restored).
fn run(scn: &Scenario) -> Vec<u8> {
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_pane_splitter_pty_harness"));
    cmd.env("PANE_HARNESS_SCREEN_MODE", scn.mode);
    cmd.env("PANE_HARNESS_AXIS", scn.axis);
    cmd.env("PANE_HARNESS_INPUT", scn.input);
    cmd.env("PANE_HARNESS_UI_HEIGHT", "12");
    cmd.env("PANE_HARNESS_EXIT_AFTER_MS", scn.exit_after_ms.to_string());
    if scn.during_probe {
        // Force a real unanswered RGB capability probe independently of the
        // invoking terminal's inherited capabilities. Input goes over the PTY
        // while the production probe reader owns it.
        for key in [
            "NO_COLOR",
            "COLORTERM",
            "TERM_PROGRAM",
            "LC_TERMINAL",
            "TMUX",
            "STY",
            "ZELLIJ",
            "WEZTERM_UNIX_SOCKET",
            "WEZTERM_PANE",
            "WEZTERM_EXECUTABLE",
            "KITTY_WINDOW_ID",
            "WT_SESSION",
        ] {
            cmd.env_remove(key);
        }
        cmd.env("FTUI_CAPS_PROBE", "1");
    }

    let mut config = PtyConfig::default()
        .with_size(PTY_COLS, PTY_ROWS)
        .with_test_name(format!("pane_input_{}_{}", scn.mode, scn.axis))
        .logging(false);
    if let Some(term) = scn.term {
        config = config.with_term(term);
    }

    let mut session = spawn_command(config, cmd).expect("spawn pane PTY harness");

    let started = Instant::now();
    let ready: &[u8] = if scn.during_probe {
        b"\x1bP+q524742\x1b\\"
    } else {
        // A title from the actual initial frame: raw mode and capability
        // probing have completed and the pane geometry has been recorded.
        b"left"
    };
    session
        .read_until(ready, Duration::from_secs(5))
        .unwrap_or_else(|err| {
            panic!(
                "[{}] readiness {ready:?} failed: {err}; output={:?}",
                scn.mode,
                String::from_utf8_lossy(session.output())
            )
        });
    for (idx, part) in scn.parts.iter().enumerate() {
        if idx > 0 && !scn.during_probe {
            std::thread::sleep(Duration::from_millis(150));
        }
        eprintln!(
            "PANE_SEND mode={} during_probe={} elapsed_us={} bytes={part:02x?}",
            scn.mode,
            scn.during_probe,
            started.elapsed().as_micros()
        );
        session.send_input(part).expect("send input chunk");
    }

    if scn.parts.last() == Some(&KEY_ESC) {
        session
            .read_until(b"cancel-ready", Duration::from_secs(5))
            .expect("actual post-cancel frame must render before stale input");
        eprintln!(
            "PANE_SEND mode={} post_cancel=true elapsed_us={} bytes={STALE_DRAG_AND_RELEASE:02x?}",
            scn.mode,
            started.elapsed().as_micros()
        );
        session
            .send_input(STALE_DRAG_AND_RELEASE)
            .expect("stale drag and release after cancel");
    }

    let status = session
        .wait_and_drain(Duration::from_secs(10))
        .expect("wait_and_drain harness");
    assert!(status.success(), "harness exited with failure: {status:?}");
    for line in String::from_utf8_lossy(session.output()).lines() {
        if let Some(start) = line
            .find("PANE_INPUT ")
            .or_else(|| line.find("PANE_RESULT "))
        {
            eprintln!("{}", &line[start..]);
        }
    }
    session.output().to_vec()
}

fn run_result(scn: &Scenario) -> PaneResult {
    parse_marker(&run(scn))
}

/// One keyboard key followed by `q`, decoded as `key` then a prompt-free quit.
fn key_then_quit(key: &'static [u8]) -> Vec<&'static [u8]> {
    vec![key, KEY_QUIT]
}

// --- Keyboard resize -------------------------------------------------------

#[test]
fn pty_keyboard_resize_is_exact_and_mode_independent() {
    // (input key, expected first-pane share in bps) on a horizontal split.
    let cases: [(&'static [u8], u32); 4] = [
        (KEY_RIGHT, INITIAL_BPS + STEP_BPS),           // 5500
        (KEY_LEFT, INITIAL_BPS - STEP_BPS),            // 4500
        (KEY_SHIFT_RIGHT, INITIAL_BPS + 5 * STEP_BPS), // 7500
        (KEY_SHIFT_LEFT, INITIAL_BPS - 5 * STEP_BPS),  // 2500
    ];
    for (key, expected) in cases {
        let mut shares = Vec::new();
        for mode in ["alt", "inline"] {
            let result = run_result(&Scenario::new(mode, key_then_quit(key)));
            assert_eq!(result.mode, mode, "marker reported the wrong screen mode");
            assert_eq!(
                result.initial_bps, INITIAL_BPS,
                "[{mode}] harness should start at a 50/50 split"
            );
            assert!(
                result.applied_ops > 0,
                "[{mode}] keyboard resize applied no operations"
            );
            assert!(
                result.tree_valid,
                "[{mode}] pane tree invalid after keyboard resize"
            );
            assert!(
                !result.canceled,
                "[{mode}] keyboard resize unexpectedly canceled"
            );
            assert_eq!(
                result.final_bps, expected,
                "[{mode}] keyboard resize landed on the wrong ratio (key={key:?})"
            );
            shares.push(result.final_bps);
        }
        // Keyboard nudges are geometry-independent: identical in both modes.
        assert_eq!(
            shares[0], shares[1],
            "keyboard resize ratio differed across screen modes (key={key:?})"
        );
    }
}

#[test]
fn pty_keyboard_plus_minus_resize() {
    // `+`/`=` increase, `-` decreases, by one step each.
    let plus = run_result(&Scenario::new("alt", key_then_quit(b"+")));
    assert!(plus.applied_ops > 0 && plus.tree_valid);
    assert_eq!(
        plus.final_bps,
        INITIAL_BPS + STEP_BPS,
        "`+` should grow the first pane"
    );

    let minus = run_result(&Scenario::new("alt", key_then_quit(b"-")));
    assert!(minus.applied_ops > 0 && minus.tree_valid);
    assert_eq!(
        minus.final_bps,
        INITIAL_BPS - STEP_BPS,
        "`-` should shrink the first pane"
    );
}

#[test]
fn pty_vertical_split_keyboard_resize() {
    // On a vertical root split, Down increases and Up decreases the first share.
    let down = run_result(&Scenario::new("alt", key_then_quit(KEY_DOWN)).axis("vertical"));
    assert!(down.applied_ops > 0, "vertical Down applied no operations");
    assert!(down.tree_valid, "tree invalid after vertical Down resize");
    assert_eq!(
        down.final_bps,
        INITIAL_BPS + STEP_BPS,
        "Down should grow the first pane"
    );

    let up = run_result(&Scenario::new("alt", key_then_quit(KEY_UP)).axis("vertical"));
    assert!(up.applied_ops > 0, "vertical Up applied no operations");
    assert!(up.tree_valid, "tree invalid after vertical Up resize");
    assert_eq!(
        up.final_bps,
        INITIAL_BPS - STEP_BPS,
        "Up should shrink the first pane"
    );
}

// --- Wheel nudge -----------------------------------------------------------

#[test]
fn pty_wheel_nudge_resize_in_both_modes() {
    for mode in ["alt", "inline"] {
        let down = run_result(&Scenario::new(
            mode,
            vec![SCROLL_DOWN_ON_SPLITTER, KEY_QUIT],
        ));
        assert!(
            down.applied_ops > 0,
            "[{mode}] scroll-down applied no operations"
        );
        assert!(down.tree_valid, "[{mode}] tree invalid after scroll-down");
        assert_eq!(
            down.final_bps,
            INITIAL_BPS + STEP_BPS,
            "[{mode}] scroll-down should nudge the first pane up one step"
        );

        let up = run_result(&Scenario::new(mode, vec![SCROLL_UP_ON_SPLITTER, KEY_QUIT]));
        assert!(
            up.applied_ops > 0,
            "[{mode}] scroll-up applied no operations"
        );
        assert!(up.tree_valid, "[{mode}] tree invalid after scroll-up");
        assert_eq!(
            up.final_bps,
            INITIAL_BPS - STEP_BPS,
            "[{mode}] scroll-up should nudge the first pane down one step"
        );
    }
}

// --- Escape recovery -------------------------------------------------------

#[test]
fn pty_escape_cancels_armed_interaction_cleanly() {
    for mode in ["alt", "inline"] {
        for during_probe in [false, true] {
            // Arm a pointer on the splitter, then send a lone ESC. The adapter must
            // route ESC to the cancel path, applying no operations and leaving the
            // tree at its initial ratio.
            let mut scenario =
                Scenario::new(mode, vec![MOUSE_DOWN_ON_SPLITTER, KEY_ESC]).exit_after_ms(4000);
            if during_probe {
                scenario = scenario.during_probe();
            }
            let output = run(&scenario);
            let result = parse_marker(&output);

            assert!(!result.timed_out, "cancel must finish before safety expiry");
            assert_eq!(
                result.recovered_bytes,
                if during_probe {
                    MOUSE_DOWN_ON_SPLITTER.len() + KEY_ESC.len()
                } else {
                    0
                },
                "scenario did not exercise the intended probe/live input path"
            );

            assert!(
                result.down_resolved,
                "[{mode}] mouse-down did not arm the splitter (hit-test failed)"
            );
            assert!(
                result.canceled,
                "[{mode}] ESC did not reach the adapter cancel path"
            );
            assert!(
                !result.committed,
                "[{mode}] canceled interaction must not commit"
            );
            assert_eq!(
                result.applied_ops, 0,
                "[{mode}] canceled interaction must apply no operations"
            );
            assert_eq!(
                result.final_bps, INITIAL_BPS,
                "[{mode}] canceled interaction must leave the ratio unchanged"
            );
            assert!(result.tree_valid, "[{mode}] pane tree invalid after cancel");
            assert!(
                result.cancel_preserved_state,
                "[{mode}] cancel changed tree/history/focus or kept pointer active"
            );
            assert!(!result.dragging_seen, "armed test unexpectedly dragged");
            assert_eq!(
                result.post_cancel_events, 2,
                "stale drag and release must reach the runtime"
            );
            assert!(
                result.post_cancel_preserved_state,
                "stale input changed canceled state"
            );
            assert!(
                result.termios_restored,
                "[{mode}] kernel termios not restored"
            );
            let options = SessionOptions {
                alternate_screen: mode == "alt",
                mouse_capture: true,
                bracketed_paste: false,
                focus_events: false,
                kitty_keyboard: false,
                intercept_signals: true,
            };
            assert_terminal_restored(&output, &CleanupExpectations::for_session(&options))
                .expect("cancel restores terminal modes");
            println!(
                "PANE_MOUSE_CLEANUP mode={mode} probe={during_probe} armed {}",
                mouse_reporting_restored(&output)
                    .expect("cancel disables every enabled mouse mode")
            );
        }
    }
}

#[test]
fn pty_escape_cancels_dragging_without_committing_or_mutating_again() {
    for mode in ["alt", "inline"] {
        for during_probe in [false, true] {
            let mut scenario = Scenario::new(
                mode,
                vec![MOUSE_DOWN_ON_SPLITTER, MOUSE_DRAG_SPLITTER_RIGHT, KEY_ESC],
            );
            if during_probe {
                scenario = scenario.during_probe();
            }
            let output = run(&scenario);
            let result = parse_marker(&output);
            assert!(!result.timed_out, "cancel must finish before safety expiry");
            assert!(
                result.down_resolved && result.dragging_seen,
                "real drag not observed: {result:?}"
            );
            assert_eq!(
                result.recovered_bytes,
                if during_probe {
                    MOUSE_DOWN_ON_SPLITTER.len() + MOUSE_DRAG_SPLITTER_RIGHT.len() + KEY_ESC.len()
                } else {
                    0
                },
                "scenario did not exercise the intended probe/live input path"
            );
            assert!(
                result.applied_ops > 0 && result.final_bps > INITIAL_BPS,
                "drag preview did not move: {result:?}"
            );
            assert!(
                result.canceled && !result.committed,
                "Escape did not cancel active drag: {result:?}"
            );
            assert!(
                result.cancel_preserved_state && result.tree_valid,
                "cancel changed tree/history/focus or retained pointer: {result:?}"
            );
            assert_eq!(
                result.post_cancel_events, 2,
                "stale drag and release must reach the runtime"
            );
            assert!(
                result.post_cancel_preserved_state,
                "stale input changed canceled state"
            );
            assert!(
                result.termios_restored,
                "kernel termios not restored: {result:?}"
            );
            let options = SessionOptions {
                alternate_screen: mode == "alt",
                mouse_capture: true,
                bracketed_paste: false,
                focus_events: false,
                kitty_keyboard: false,
                intercept_signals: true,
            };
            assert_terminal_restored(&output, &CleanupExpectations::for_session(&options))
                .expect("drag cancel restores terminal modes");
            println!(
                "PANE_MOUSE_CLEANUP mode={mode} probe={during_probe} dragging {}",
                mouse_reporting_restored(&output)
                    .expect("cancel disables every enabled mouse mode")
            );
        }
    }
}

#[test]
fn pty_cancel_preserves_real_undo_history_and_keyboard_pane_focus() {
    for mode in ["alt", "inline"] {
        for during_probe in [false, true] {
            for dragging in [false, true] {
                // Two actual resize commands create nonempty undo history,
                // restoring the splitter before the subsequent pointer press.
                // Tab then changes actual pane focus from left to right.
                let mut parts = vec![KEY_RIGHT, KEY_LEFT, KEY_TAB, MOUSE_DOWN_ON_SPLITTER];
                if dragging {
                    parts.push(MOUSE_DRAG_SPLITTER_RIGHT);
                }
                parts.push(KEY_ESC);
                let recovered_bytes = parts.iter().map(|part| part.len()).sum::<usize>();
                let mut scenario = Scenario::new(mode, parts).exit_after_ms(4000);
                if during_probe {
                    scenario = scenario.during_probe();
                }
                let output = run(&scenario);
                let result = parse_marker(&output);
                assert!(!result.timed_out, "cancel must finish before safety expiry");
                assert_eq!(
                    result.recovered_bytes,
                    if during_probe { recovered_bytes } else { 0 }
                );
                assert!(
                    result.down_resolved && result.canceled && !result.committed,
                    "{result:?}"
                );
                assert_eq!(result.dragging_seen, dragging, "{result:?}");
                assert_eq!(
                    result.active_pane, "right",
                    "Tab focus did not survive cancel: {result:?}"
                );
                assert!(!result.maximized);
                assert_eq!(
                    result.history_entries,
                    if dragging { 3 } else { 2 },
                    "{result:?}"
                );
                assert_eq!(result.history_cursor, result.history_entries, "{result:?}");
                assert!(
                    result.cancel_preserved_state && result.post_cancel_preserved_state,
                    "{result:?}"
                );
                assert_eq!(result.post_cancel_events, 2);
                assert!(
                    result.cancel_history_undo_redo,
                    "actual undo/redo must change and restore tree: {result:?}"
                );
                assert!(result.tree_valid && result.termios_restored, "{result:?}");
                if dragging {
                    assert!(result.final_bps > INITIAL_BPS);
                } else {
                    assert_eq!(result.final_bps, INITIAL_BPS);
                }
                let options = SessionOptions {
                    alternate_screen: mode == "alt",
                    mouse_capture: true,
                    bracketed_paste: false,
                    focus_events: false,
                    kitty_keyboard: false,
                    intercept_signals: true,
                };
                assert_terminal_restored(&output, &CleanupExpectations::for_session(&options))
                    .expect("history/focus cancel restores terminal modes");
                println!(
                    "PANE_MOUSE_CLEANUP mode={mode} probe={during_probe} history dragging={dragging} {}",
                    mouse_reporting_restored(&output)
                        .expect("cancel disables every enabled mouse mode")
                );
            }
        }
    }
}

#[test]
fn pty_safety_timeout_restores_terminal_with_an_uncommitted_pointer() {
    for mode in ["alt", "inline"] {
        for dragging in [false, true] {
            let mut parts = vec![MOUSE_DOWN_ON_SPLITTER];
            if dragging {
                parts.push(MOUSE_DRAG_SPLITTER_RIGHT);
            }
            // No Escape, release or quit: only the actual safety heartbeat can
            // end this session. Its exit marker distinguishes that path.
            let output = run(&Scenario::new(mode, parts).exit_after_ms(2000));
            let result = parse_marker(&output);
            assert!(result.timed_out && result.down_resolved, "{result:?}");
            assert_eq!(result.dragging_seen, dragging, "{result:?}");
            assert!(!result.canceled && !result.committed, "{result:?}");
            assert_eq!(result.applied_ops, u64::from(dragging), "{result:?}");
            assert_eq!(result.history_entries, usize::from(dragging), "{result:?}");
            assert_eq!(result.history_cursor, result.history_entries, "{result:?}");
            assert_eq!(result.active_pane, "left", "{result:?}");
            assert!(!result.maximized);
            assert_eq!(result.post_cancel_events, 0);
            assert_eq!(result.recovered_bytes, 0);
            assert!(result.tree_valid && result.termios_restored, "{result:?}");
            if dragging {
                assert!(result.final_bps > INITIAL_BPS);
            } else {
                assert_eq!(result.final_bps, INITIAL_BPS);
            }
            let options = SessionOptions {
                alternate_screen: mode == "alt",
                mouse_capture: true,
                bracketed_paste: false,
                focus_events: false,
                kitty_keyboard: false,
                intercept_signals: true,
            };
            assert_terminal_restored(&output, &CleanupExpectations::for_session(&options))
                .expect("safety expiry restores terminal modes");
            println!(
                "PANE_MOUSE_CLEANUP mode={mode} timeout=true dragging={dragging} {}",
                mouse_reporting_restored(&output)
                    .expect("safety expiry disables every enabled mouse mode")
            );
        }
    }
}

// --- Structural operations (split / close / swap) over a real PTY ----------

#[test]
fn pty_split_left_leaf_grows_the_tree() {
    let result = run_result(&Scenario::new("alt", vec![KEY_SPLIT]));
    assert!(result.applied_ops > 0, "split applied no operations");
    assert!(result.tree_valid, "tree invalid after split");
    // Root split + new split + 3 leaves = 5 nodes (was 3).
    assert_eq!(
        result.node_count, 5,
        "split should add a split parent and a leaf"
    );
}

#[test]
fn pty_close_right_leaf_shrinks_the_tree() {
    let result = run_result(&Scenario::new("alt", vec![KEY_CLOSE]));
    assert!(result.applied_ops > 0, "close applied no operations");
    assert!(result.tree_valid, "tree invalid after close");
    // Closing the right leaf promotes the left leaf to root: a single node.
    assert_eq!(
        result.node_count, 1,
        "close should promote the surviving sibling to root"
    );
}

#[test]
fn pty_swap_reorders_leaves() {
    // Baseline: the first child is "left".
    let baseline = run_result(&Scenario::new("alt", key_then_quit(KEY_RIGHT)));
    assert_eq!(
        baseline.first_leaf, "left",
        "baseline first child should be the left leaf"
    );

    let swapped = run_result(&Scenario::new("alt", vec![KEY_SWAP]));
    assert!(swapped.applied_ops > 0, "swap applied no operations");
    assert!(swapped.tree_valid, "tree invalid after swap");
    assert_eq!(swapped.node_count, 3, "swap must not change the node count");
    assert_eq!(
        swapped.first_leaf, "right",
        "swap should move the right leaf into the first slot"
    );
}

// --- Capability matrix -----------------------------------------------------

#[test]
fn pty_keyboard_resize_across_terminal_capability_matrix() {
    // The deterministic keyboard resize must produce the same ratio and a clean
    // teardown regardless of the terminal capability profile.
    for term in ["xterm-256color", "screen", "tmux-256color"] {
        let output = run(&Scenario::new("alt", key_then_quit(KEY_RIGHT)).term(term));
        let result = parse_marker(&output);

        assert!(
            result.applied_ops > 0,
            "[{term}] keyboard resize applied no operations"
        );
        assert!(result.tree_valid, "[{term}] pane tree invalid after resize");
        assert_eq!(
            result.final_bps,
            INITIAL_BPS + STEP_BPS,
            "[{term}] keyboard resize landed on the wrong ratio"
        );

        // Conservative, capability-independent teardown expectations: cursor is
        // restored and alt-screen is exited. Mouse/paste/focus disable sequences
        // are sanitized against detected capabilities, so we do not assert them.
        let options = SessionOptions {
            alternate_screen: true,
            mouse_capture: false,
            bracketed_paste: false,
            focus_events: false,
            kitty_keyboard: false,
            intercept_signals: true,
        };
        let expectations = CleanupExpectations::for_session(&options);
        assert_terminal_restored(&output, &expectations)
            .unwrap_or_else(|err| panic!("[{term}] terminal cleanup verification failed: {err}"));
    }
}

// --- Production keyboard bindings over PTY (bd-8e1oc) -----------------------
//
// These drive the REAL terminal keyboard binding (`ftui_runtime::pane_keymap`),
// not the harness affordance keys, via `PANE_HARNESS_INPUT=keymap`. The harness
// starts focused on the left leaf; each scenario sends a key sequence then `q`,
// and the marker reports `active_pane` (keyboard focus navigation) plus the
// structural state (`node_count`, `first_leaf`, `maximized`).

/// Run a keymap-mode scenario (key sequence followed by `q`) and parse the marker.
fn keymap_result(mode: &'static str, key: &'static [u8]) -> PaneResult {
    run_result(&Scenario::new(mode, vec![key, KEY_QUIT]).keymap())
}

#[test]
fn pty_keymap_tab_navigates_focus_to_next_pane() {
    // Start focus = left; Tab -> FocusNext -> right. Mode-independent.
    for mode in ["alt", "inline"] {
        let result = keymap_result(mode, KEY_TAB);
        assert!(result.tree_valid, "[{mode}] tree invalid after focus nav");
        assert_eq!(
            result.active_pane, "right",
            "[{mode}] Tab should move keyboard focus to the next pane"
        );
        // Focus navigation is not a structural change.
        assert_eq!(
            result.node_count, 3,
            "[{mode}] focus nav must not mutate the tree"
        );
        assert!(!result.maximized);
    }
}

#[test]
fn pty_keymap_ctrl_arrow_directional_focus() {
    // Ctrl+Right -> FocusDirectional(Right): left -> right pane.
    let result = keymap_result("alt", KEY_CTRL_RIGHT);
    assert!(result.tree_valid);
    assert_eq!(
        result.active_pane, "right",
        "Ctrl+Right should focus the pane to the right"
    );
    assert_eq!(result.node_count, 3);
}

#[test]
fn pty_keymap_alt_split_grows_the_tree() {
    // Alt+s -> Split(Horizontal) on the active (left) leaf: the tree grows.
    let result = keymap_result("alt", KEY_ALT_S);
    assert!(result.tree_valid, "tree invalid after keyboard split");
    assert!(
        result.node_count > 3,
        "Alt+s should split the active pane (node_count={})",
        result.node_count
    );
    assert!(result.applied_ops > 0, "split should apply operations");
}

#[test]
fn pty_keymap_alt_close_shrinks_the_tree() {
    // Alt+w -> Close the active (left) leaf: the sibling is promoted, focus moves.
    let result = keymap_result("alt", KEY_ALT_W);
    assert!(result.tree_valid, "tree invalid after keyboard close");
    assert!(
        result.node_count < 3,
        "Alt+w should close the active pane (node_count={})",
        result.node_count
    );
    assert_ne!(
        result.active_pane, "left",
        "focus must move off the closed pane"
    );
}

#[test]
fn pty_keymap_alt_maximize_sets_transient_state() {
    // Alt+z -> Maximize the active pane: transient view state, no topology change.
    let result = keymap_result("alt", KEY_ALT_Z);
    assert!(result.tree_valid);
    assert!(result.maximized, "Alt+z should maximize the active pane");
    assert_eq!(result.node_count, 3, "maximize must not mutate the tree");
}

#[test]
fn pty_keymap_focus_nav_across_terminal_capability_matrix() {
    // Keyboard focus navigation (Tab) under several TERM profiles, with clean
    // teardown each time — proving the production binding works across emulators.
    for term in ["xterm-256color", "screen", "tmux-256color"] {
        let options = SessionOptions::default();
        let scn = Scenario::new("alt", vec![KEY_TAB, KEY_QUIT])
            .keymap()
            .term(term)
            .exit_after_ms(3000);
        let output = run(&scn);
        let result = parse_marker(&output);
        assert_eq!(
            result.active_pane, "right",
            "[{term}] Tab focus navigation should reach the next pane"
        );
        assert!(result.tree_valid, "[{term}] tree invalid");

        let expectations = CleanupExpectations::for_session(&options);
        assert_terminal_restored(&output, &expectations)
            .unwrap_or_else(|err| panic!("[{term}] terminal cleanup verification failed: {err}"));
    }
}
