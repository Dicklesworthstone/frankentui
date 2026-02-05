#![forbid(unsafe_code)]
#![cfg(unix)]

use std::time::Duration;

use ftui_core::terminal_session::SessionOptions;
use ftui_pty::{CleanupExpectations, PtyConfig, assert_terminal_restored, spawn_command};
use portable_pty::CommandBuilder;

const CURSOR_SAVE: &[u8] = b"\x1b7";
const CURSOR_RESTORE: &[u8] = b"\x1b8";

fn find_sequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn run_harness(screen_mode: &str) -> Vec<u8> {
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ftui-harness"));
    cmd.env("FTUI_HARNESS_EXIT_AFTER_MS", "120");
    cmd.env("FTUI_HARNESS_SCREEN_MODE", screen_mode);
    cmd.env("FTUI_HARNESS_UI_HEIGHT", "6");
    cmd.env("FTUI_HARNESS_LOG_LINES", "3");
    cmd.env("FTUI_HARNESS_SUPPRESS_WELCOME", "1");

    let config = PtyConfig::default()
        .with_size(80, 24)
        .with_test_name(format!("harness_{screen_mode}_lifecycle"))
        .logging(false);

    let mut session = spawn_command(config, cmd).expect("spawn harness in PTY");
    let status = session
        .wait_and_drain(Duration::from_secs(4))
        .expect("wait_and_drain");
    assert!(status.success(), "harness exited with failure: {status:?}");
    session.output().to_vec()
}

#[test]
fn pty_inline_mode_restores_terminal_and_uses_cursor_save_restore() {
    let output = run_harness("inline");
    assert!(
        !output.is_empty(),
        "expected non-empty PTY output from harness"
    );

    let options = SessionOptions {
        alternate_screen: false,
        mouse_capture: false,
        bracketed_paste: true,
        focus_events: false,
        kitty_keyboard: false,
    };
    let expectations = CleanupExpectations::for_session(&options);
    assert_terminal_restored(&output, &expectations)
        .expect("inline mode terminal cleanup verification failed");

    let save_idx = find_sequence(&output, CURSOR_SAVE).expect("missing cursor save");
    let restore_idx = find_sequence(&output, CURSOR_RESTORE).expect("missing cursor restore");
    assert!(
        save_idx < restore_idx,
        "cursor restore must appear after save (save={save_idx}, restore={restore_idx})"
    );
}

#[test]
fn pty_alt_screen_restores_terminal() {
    let output = run_harness("alt");
    assert!(
        !output.is_empty(),
        "expected non-empty PTY output from harness"
    );

    let options = SessionOptions {
        alternate_screen: true,
        mouse_capture: false,
        bracketed_paste: true,
        focus_events: false,
        kitty_keyboard: false,
    };
    let expectations = CleanupExpectations::for_session(&options);
    assert_terminal_restored(&output, &expectations)
        .expect("alt-screen terminal cleanup verification failed");
}
