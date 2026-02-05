#![forbid(unsafe_code)]

//! End-to-end tests for embedded terminal (ftui-pty) functionality (bd-2ueu.6).
//!
//! Exercises four key integration paths:
//!
//! 1. **PTY Management**: Shell spawn, env inheritance, working dir, clean termination
//! 2. **ANSI Rendering**: VirtualTerminal color, cursor, scrollback, wrapping
//! 3. **Input Forwarding**: key_to_sequence → VirtualTerminal pipeline
//! 4. **Resize Handling**: Terminal size variations and boundary conditions
//!
//! Run: `cargo test -p ftui-demo-showcase --test terminal_e2e`

use std::time::Duration;

use ftui_pty::input_forwarding::{
    BracketedPaste, InputForwarder, Key, KeyEvent, Modifiers, key_to_sequence,
};
use ftui_pty::virtual_terminal::{Color, QuirkSet, VirtualTerminal};

// =============================================================================
// JSONL Logging
// =============================================================================

fn log_jsonl(test: &str, check: &str, passed: bool, notes: &str) {
    eprintln!(
        "{{\"test\":\"{test}\",\"check\":\"{check}\",\"passed\":{passed},\"notes\":\"{notes}\"}}"
    );
}

// =============================================================================
// Scenario 1: PTY Management (unix-only, requires real PTY)
// =============================================================================

#[cfg(unix)]
mod pty_management {
    use super::*;
    use ftui_pty::pty_process::{PtyProcess, ShellConfig};
    use ftui_pty::{PtyConfig, spawn_command};
    use portable_pty::CommandBuilder;

    #[test]
    fn e2e_shell_spawn_and_output_capture() {
        let config = PtyConfig::default()
            .with_test_name("shell_spawn_capture")
            .logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'HELLO_E2E_MARKER'"]);

        let mut session = spawn_command(config, cmd).expect("spawn should succeed");
        let output = session
            .read_until(b"HELLO_E2E_MARKER", Duration::from_secs(5))
            .expect("marker should appear in output");

        let found = output
            .windows(b"HELLO_E2E_MARKER".len())
            .any(|w| w == b"HELLO_E2E_MARKER");
        log_jsonl("pty_spawn", "output_capture", found, "");
        assert!(found, "PTY output must contain the marker string");
    }

    #[test]
    fn e2e_environment_inheritance() {
        let config = PtyConfig::default()
            .with_env("FTUI_TEST_SECRET", "xyzzy_42")
            .with_test_name("env_inherit")
            .logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf $FTUI_TEST_SECRET"]);

        let mut session = spawn_command(config, cmd).expect("spawn should succeed");
        let output = session
            .read_until(b"xyzzy_42", Duration::from_secs(5))
            .expect("env var should appear in output");

        let found = output.windows(b"xyzzy_42".len()).any(|w| w == b"xyzzy_42");
        log_jsonl("pty_env", "inheritance", found, "");
        assert!(found, "Child must inherit custom env vars");
    }

    #[test]
    fn e2e_working_directory() {
        let config = ShellConfig::default().cwd("/tmp").logging(false);

        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");
        proc.write_all(b"pwd\n").expect("write should succeed");

        let output = proc
            .read_until(b"/tmp", Duration::from_secs(5))
            .expect("pwd should show /tmp");

        let out_str = String::from_utf8_lossy(&output);
        let has_tmp = out_str.contains("/tmp");
        log_jsonl("pty_cwd", "working_dir", has_tmp, "");
        assert!(has_tmp, "Shell must start in configured working directory");

        proc.kill().expect("kill should succeed");
    }

    #[test]
    fn e2e_clean_termination_exit() {
        let config = ShellConfig::default().logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        assert!(proc.is_alive(), "Process should be alive after spawn");

        proc.write_all(b"exit 0\n").expect("write should succeed");
        let status = proc
            .wait_timeout(Duration::from_secs(5))
            .expect("wait should succeed");

        let success = status.success();
        log_jsonl("pty_clean_exit", "exit_0", success, "");
        assert!(success, "exit 0 should produce success status");
        assert!(!proc.is_alive(), "Process should be dead after exit");
    }

    #[test]
    fn e2e_clean_termination_kill() {
        let config = ShellConfig::default().logging(false);
        let mut proc = PtyProcess::spawn(config).expect("spawn should succeed");

        assert!(proc.is_alive());
        proc.kill().expect("kill should succeed");

        let dead = !proc.is_alive();
        log_jsonl("pty_kill", "terminated", dead, "");
        assert!(dead, "Process should be dead after kill");

        // Kill is idempotent
        proc.kill().expect("second kill should succeed");
        log_jsonl("pty_kill", "idempotent", true, "");
    }

    #[test]
    fn e2e_pty_session_wait_and_drain() {
        let config = PtyConfig::default()
            .with_test_name("wait_drain")
            .logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args([
            "-c",
            "printf 'A\\n'; sleep 0.02; printf 'B\\n'; sleep 0.02; printf 'C\\n'",
        ]);

        let mut session = spawn_command(config, cmd).expect("spawn should succeed");
        let status = session
            .wait_and_drain(Duration::from_secs(3))
            .expect("wait_and_drain should succeed");

        assert!(status.success(), "Child should exit successfully");

        let out = String::from_utf8_lossy(session.output());
        let has_all = out.contains('A') && out.contains('B') && out.contains('C');
        log_jsonl("pty_drain", "all_output", has_all, "");
        assert!(has_all, "All output lines must be captured after drain");
    }

    #[test]
    fn e2e_term_env_propagation() {
        let config = PtyConfig::default()
            .with_term("dumb")
            .with_test_name("term_env")
            .logging(false);

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf $TERM"]);

        let mut session = spawn_command(config, cmd).expect("spawn should succeed");
        let output = session
            .read_until(b"dumb", Duration::from_secs(5))
            .expect("TERM should appear in output");

        let found = output.windows(4).any(|w| w == b"dumb");
        log_jsonl("pty_term", "propagation", found, "");
        assert!(found, "TERM env var must propagate to child");
    }
}

// =============================================================================
// Scenario 2: ANSI Rendering via VirtualTerminal
// =============================================================================

mod ansi_rendering {
    use super::*;

    #[test]
    fn e2e_256_color_foreground() {
        let mut vt = VirtualTerminal::new(80, 24);
        // Set fg to color index 196 (bright red in 256-color palette)
        vt.feed(b"\x1b[38;5;196mR");
        let style = vt.style_at(0, 0).unwrap();

        let has_fg = style.fg.is_some();
        log_jsonl("ansi_256", "fg_set", has_fg, "index=196");
        assert!(has_fg, "256-color foreground must be set");
    }

    #[test]
    fn e2e_256_color_background() {
        let mut vt = VirtualTerminal::new(80, 24);
        // Set bg to color index 21 (blue in 256-color palette)
        vt.feed(b"\x1b[48;5;21mB");
        let style = vt.style_at(0, 0).unwrap();

        let has_bg = style.bg.is_some();
        log_jsonl("ansi_256", "bg_set", has_bg, "index=21");
        assert!(has_bg, "256-color background must be set");
    }

    #[test]
    fn e2e_truecolor_rgb() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[38;2;100;200;50mG");
        let style = vt.style_at(0, 0).unwrap();

        let correct = style.fg == Some(Color::new(100, 200, 50));
        log_jsonl("ansi_truecolor", "rgb_fg", correct, "100,200,50");
        assert!(correct, "Truecolor RGB must be correctly parsed");
    }

    #[test]
    fn e2e_cursor_positioning() {
        let mut vt = VirtualTerminal::new(80, 24);
        // Move to row 5, col 10 (1-indexed)
        vt.feed(b"\x1b[5;10H");
        assert_eq!(vt.cursor(), (9, 4), "CUP should set 0-indexed position");

        // Move to row 1, col 1
        vt.feed(b"\x1b[H");
        assert_eq!(vt.cursor(), (0, 0), "CUP without params defaults to 1;1");

        // Write and verify position advances
        vt.feed(b"XYZ");
        assert_eq!(vt.cursor(), (3, 0), "Cursor advances after text output");
        assert_eq!(vt.char_at(0, 0), Some('X'));
        assert_eq!(vt.char_at(2, 0), Some('Z'));

        log_jsonl("ansi_cursor", "positioning", true, "");
    }

    #[test]
    fn e2e_cursor_movement_sequence() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[10;20H"); // (19, 9)

        // Up 3
        vt.feed(b"\x1b[3A");
        assert_eq!(vt.cursor(), (19, 6));

        // Down 5
        vt.feed(b"\x1b[5B");
        assert_eq!(vt.cursor(), (19, 11));

        // Forward 10
        vt.feed(b"\x1b[10C");
        assert_eq!(vt.cursor(), (29, 11));

        // Back 15
        vt.feed(b"\x1b[15D");
        assert_eq!(vt.cursor(), (14, 11));

        log_jsonl("ansi_cursor", "movement", true, "");
    }

    #[test]
    fn e2e_scrollback_buffer() {
        let mut vt = VirtualTerminal::new(20, 3);
        // Write 5 lines into a 3-line terminal
        vt.feed(b"Line1\r\nLine2\r\nLine3\r\nLine4\r\nLine5");

        // Screen should show lines 3-5, scrollback has lines 1-2
        assert_eq!(vt.row_text(0), "Line3");
        assert_eq!(vt.row_text(1), "Line4");
        assert_eq!(vt.row_text(2), "Line5");
        assert_eq!(vt.scrollback_len(), 2);
        assert_eq!(vt.scrollback_line(0), Some("Line1".to_string()));
        assert_eq!(vt.scrollback_line(1), Some("Line2".to_string()));

        log_jsonl("ansi_scrollback", "overflow", true, "");
    }

    #[test]
    fn e2e_scrollback_truncation() {
        let mut vt = VirtualTerminal::new(10, 2);
        vt.set_max_scrollback(3);

        // Push 10 lines into a 2-line terminal with 3-line scrollback
        for i in 0..10 {
            vt.feed_str(&format!("L{i}\n"));
        }

        let within_limit = vt.scrollback_len() <= 3;
        log_jsonl("ansi_scrollback", "truncation", within_limit, "max=3");
        assert!(within_limit, "Scrollback must be truncated to max");
    }

    #[test]
    fn e2e_line_wrapping() {
        let mut vt = VirtualTerminal::new(5, 4);
        vt.feed(b"ABCDEFGHIJ");

        assert_eq!(vt.row_text(0), "ABCDE");
        assert_eq!(vt.row_text(1), "FGHIJ");
        // Cursor is at (5,1) — past last column due to deferred wrap.
        // VirtualTerminal defers wrap until the next character is written.
        assert_eq!(vt.cursor(), (5, 1));

        log_jsonl("ansi_wrap", "auto_wrap", true, "width=5");
    }

    #[test]
    fn e2e_line_wrapping_fills_screen() {
        let mut vt = VirtualTerminal::new(5, 3);
        // Write enough to fill and scroll
        vt.feed(b"AAAAABBBBBCCCCCDDDDDEEEEE");

        // Last 3 rows visible, first rows scrolled
        assert_eq!(vt.row_text(0), "CCCCC");
        assert_eq!(vt.row_text(1), "DDDDD");
        assert_eq!(vt.row_text(2), "EEEEE");
        assert!(vt.scrollback_len() >= 2);

        log_jsonl("ansi_wrap", "fill_scroll", true, "");
    }

    #[test]
    fn e2e_erase_display_and_line() {
        let mut vt = VirtualTerminal::new(20, 3);
        vt.feed(b"AAAA\r\nBBBB\r\nCCCC");

        // Erase from cursor (end of C row) to end
        vt.feed(b"\x1b[2;3H"); // row 2, col 3 (0-indexed: 2, 1)
        vt.feed(b"\x1b[J"); // Erase from cursor

        assert_eq!(vt.row_text(0), "AAAA");
        assert_eq!(vt.row_text(1), "BB");
        assert_eq!(vt.row_text(2), "");

        log_jsonl("ansi_erase", "from_cursor", true, "");
    }

    #[test]
    fn e2e_sgr_attributes() {
        let mut vt = VirtualTerminal::new(80, 24);
        // Bold + italic + underline + red fg + blue bg
        vt.feed(b"\x1b[1;3;4;31;44mSTYLED\x1b[0mPLAIN");

        let styled = vt.style_at(0, 0).unwrap();
        assert!(styled.bold, "Bold should be set");
        assert!(styled.italic, "Italic should be set");
        assert!(styled.underline, "Underline should be set");
        assert_eq!(styled.fg, Some(Color::new(170, 0, 0)), "Red fg");
        assert_eq!(styled.bg, Some(Color::new(0, 0, 170)), "Blue bg");

        let plain = vt.style_at(6, 0).unwrap();
        assert!(!plain.bold, "Bold should be reset");
        assert!(plain.fg.is_none(), "Fg should be reset");

        log_jsonl("ansi_sgr", "attributes", true, "");
    }

    #[test]
    fn e2e_alternate_screen() {
        let mut vt = VirtualTerminal::new(20, 3);
        vt.feed(b"MAIN_TEXT");
        assert_eq!(vt.row_text(0), "MAIN_TEXT");
        assert!(!vt.is_alternate_screen());

        // Enter alt screen
        vt.feed(b"\x1b[?1049h");
        assert!(vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), "", "Alt screen starts blank");

        vt.feed(b"ALT_TEXT");
        assert_eq!(vt.row_text(0), "ALT_TEXT");

        // Exit alt screen
        vt.feed(b"\x1b[?1049l");
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.row_text(0), "MAIN_TEXT", "Main screen restored");

        log_jsonl("ansi_altscreen", "roundtrip", true, "");
    }

    #[test]
    fn e2e_osc_title() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b]0;My Terminal Title\x07");

        let correct = vt.title() == "My Terminal Title";
        log_jsonl("ansi_osc", "title", correct, "");
        assert!(correct, "OSC title must be parsed");
    }

    #[test]
    fn e2e_cursor_visibility() {
        let mut vt = VirtualTerminal::new(80, 24);
        assert!(vt.cursor_visible());

        vt.feed(b"\x1b[?25l");
        assert!(!vt.cursor_visible(), "Cursor should be hidden");

        vt.feed(b"\x1b[?25h");
        assert!(vt.cursor_visible(), "Cursor should be visible again");

        log_jsonl("ansi_cursor", "visibility", true, "");
    }

    #[test]
    fn e2e_scroll_region() {
        let mut vt = VirtualTerminal::new(20, 5);
        // Set scroll region to rows 2-4 (1-indexed)
        vt.feed(b"\x1b[2;4r");
        // Fill all rows
        vt.feed(b"\x1b[1;1HROW1");
        vt.feed(b"\x1b[2;1HROW2");
        vt.feed(b"\x1b[3;1HROW3");
        vt.feed(b"\x1b[4;1HROW4");
        vt.feed(b"\x1b[5;1HROW5");

        assert_eq!(vt.row_text(0), "ROW1");
        assert_eq!(vt.row_text(4), "ROW5");

        log_jsonl("ansi_scroll_region", "set", true, "");
    }

    #[test]
    fn e2e_dec_save_restore_cursor() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[10;20H"); // Move to (19, 9)
        vt.feed(b"\x1b7"); // Save
        vt.feed(b"\x1b[1;1H"); // Move to origin
        assert_eq!(vt.cursor(), (0, 0));
        vt.feed(b"\x1b8"); // Restore
        assert_eq!(vt.cursor(), (19, 9), "Cursor must be restored");

        log_jsonl("ansi_dec", "save_restore", true, "");
    }

    #[test]
    fn e2e_full_reset() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"Some text\x1b[1;31m");
        vt.feed(b"\x1b[10;10H");
        vt.feed(b"\x1bc"); // RIS

        assert_eq!(vt.cursor(), (0, 0));
        assert_eq!(vt.row_text(0), "");
        assert!(vt.cursor_visible());

        log_jsonl("ansi_reset", "ris", true, "");
    }
}

// =============================================================================
// Scenario 3: Input Forwarding Integration
// =============================================================================

mod input_forwarding_integration {
    use super::*;

    #[test]
    fn e2e_key_to_vt_plain_chars() {
        let mut vt = VirtualTerminal::new(80, 24);

        // Type "Hello" via key_to_sequence → VirtualTerminal
        for ch in "Hello".chars() {
            let seq = key_to_sequence(KeyEvent::plain(Key::Char(ch)));
            vt.feed(&seq);
        }

        assert_eq!(vt.row_text(0), "Hello");
        log_jsonl("input_vt", "plain_chars", true, "");
    }

    #[test]
    fn e2e_key_to_vt_enter_newline() {
        let mut vt = VirtualTerminal::new(80, 24);

        // Type "AB", press Enter, type "CD"
        for ch in "AB".chars() {
            vt.feed(&key_to_sequence(KeyEvent::plain(Key::Char(ch))));
        }
        // Enter sends CR; feed LF to advance line in terminal
        vt.feed(&key_to_sequence(KeyEvent::plain(Key::Enter)));
        vt.feed(b"\n");
        for ch in "CD".chars() {
            vt.feed(&key_to_sequence(KeyEvent::plain(Key::Char(ch))));
        }

        assert_eq!(vt.row_text(0), "AB");
        assert_eq!(vt.row_text(1), "CD");
        log_jsonl("input_vt", "enter_newline", true, "");
    }

    #[test]
    fn e2e_key_to_vt_arrow_movement() {
        let mut vt = VirtualTerminal::new(80, 24);

        // Position cursor at (10, 10) using CUP
        vt.feed(b"\x1b[11;11H");
        assert_eq!(vt.cursor(), (10, 10));

        // Up arrow
        let seq = key_to_sequence(KeyEvent::plain(Key::Up));
        vt.feed(&seq);
        assert_eq!(vt.cursor(), (10, 9), "Up arrow should move cursor up");

        // Down arrow
        let seq = key_to_sequence(KeyEvent::plain(Key::Down));
        vt.feed(&seq);
        assert_eq!(vt.cursor(), (10, 10), "Down arrow should move cursor down");

        // Right arrow
        let seq = key_to_sequence(KeyEvent::plain(Key::Right));
        vt.feed(&seq);
        assert_eq!(
            vt.cursor(),
            (11, 10),
            "Right arrow should move cursor right"
        );

        // Left arrow
        let seq = key_to_sequence(KeyEvent::plain(Key::Left));
        vt.feed(&seq);
        assert_eq!(vt.cursor(), (10, 10), "Left arrow should move cursor left");

        log_jsonl("input_vt", "arrow_movement", true, "");
    }

    #[test]
    fn e2e_key_to_vt_home_end() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[5;20H"); // Position at (19, 4)

        let seq = key_to_sequence(KeyEvent::plain(Key::Home));
        vt.feed(&seq);
        // Home sends CSI H which is CUP → (0,0)
        assert_eq!(vt.cursor(), (0, 0), "Home sends CUP to origin");

        log_jsonl("input_vt", "home_end", true, "");
    }

    #[test]
    fn e2e_modifier_combinations() {
        // Ctrl+C → ETX (0x03)
        let seq = key_to_sequence(KeyEvent::new(Key::Char('c'), Modifiers::CTRL));
        assert_eq!(seq, vec![0x03]);

        // Alt+x → ESC x
        let seq = key_to_sequence(KeyEvent::new(Key::Char('x'), Modifiers::ALT));
        assert_eq!(seq, vec![0x1b, b'x']);

        // Ctrl+Alt+c → ESC + ETX
        let seq = key_to_sequence(KeyEvent::new(
            Key::Char('c'),
            Modifiers {
                ctrl: true,
                alt: true,
                shift: false,
            },
        ));
        assert_eq!(seq, vec![0x1b, 0x03]);

        // Shift+a → A
        let seq = key_to_sequence(KeyEvent::new(Key::Char('a'), Modifiers::SHIFT));
        assert_eq!(seq, b"A");

        log_jsonl("input_modifiers", "combinations", true, "");
    }

    #[test]
    fn e2e_ctrl_arrow_with_modifier_param() {
        // Ctrl+Up → ESC [ 1 ; 5 A
        let seq = key_to_sequence(KeyEvent::new(Key::Up, Modifiers::CTRL));
        assert_eq!(seq, b"\x1b[1;5A");

        // Shift+Down → ESC [ 1 ; 2 B
        let seq = key_to_sequence(KeyEvent::new(Key::Down, Modifiers::SHIFT));
        assert_eq!(seq, b"\x1b[1;2B");

        // Alt+Left → ESC [ 1 ; 3 D
        let seq = key_to_sequence(KeyEvent::new(Key::Left, Modifiers::ALT));
        assert_eq!(seq, b"\x1b[1;3D");

        log_jsonl("input_modifiers", "arrow_params", true, "");
    }

    #[test]
    fn e2e_function_keys() {
        // F1 → ESC O P
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::F(1))), b"\x1bOP");
        // F5 → ESC [ 15 ~
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::F(5))), b"\x1b[15~");
        // F12 → ESC [ 24 ~
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::F(12))), b"\x1b[24~");
        // Shift+F1 → ESC [ 1 ; 2 P
        assert_eq!(
            key_to_sequence(KeyEvent::new(Key::F(1), Modifiers::SHIFT)),
            b"\x1b[1;2P"
        );
        // Ctrl+F5 → ESC [ 15 ; 5 ~
        assert_eq!(
            key_to_sequence(KeyEvent::new(Key::F(5), Modifiers::CTRL)),
            b"\x1b[15;5~"
        );
        // Out-of-range F key → empty
        assert!(key_to_sequence(KeyEvent::plain(Key::F(13))).is_empty());

        log_jsonl("input_fkeys", "all_ranges", true, "");
    }

    #[test]
    fn e2e_bracketed_paste() {
        let mut bp = BracketedPaste::new();
        assert!(!bp.is_enabled());

        // Without bracketing: raw text
        let unwrapped = bp.wrap(b"pasted text");
        assert_eq!(unwrapped, b"pasted text");

        // Enable bracketing
        bp.enable();
        assert!(bp.is_enabled());

        let wrapped = bp.wrap(b"pasted text");
        assert!(wrapped.starts_with(BracketedPaste::START));
        assert!(wrapped.ends_with(BracketedPaste::END));
        let inner_start = BracketedPaste::START.len();
        let inner_end = wrapped.len() - BracketedPaste::END.len();
        assert_eq!(&wrapped[inner_start..inner_end], b"pasted text");

        // Disable and verify
        bp.disable();
        assert!(!bp.is_enabled());
        assert_eq!(bp.wrap(b"raw"), b"raw");

        log_jsonl("input_paste", "bracketing", true, "");
    }

    #[test]
    fn e2e_input_forwarder_pipeline() {
        let mut buffer = Vec::new();
        {
            let mut fwd = InputForwarder::new(&mut buffer);
            fwd.forward_key(KeyEvent::plain(Key::Char('A'))).unwrap();
            fwd.forward_key(KeyEvent::plain(Key::Enter)).unwrap();
            fwd.forward_key(KeyEvent::new(Key::Char('c'), Modifiers::CTRL))
                .unwrap();
        }

        assert_eq!(buffer, vec![b'A', 0x0d, 0x03]);

        // Feed the buffer through a VirtualTerminal
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(&buffer);
        assert_eq!(vt.char_at(0, 0), Some('A'));

        log_jsonl("input_forwarder", "pipeline", true, "");
    }

    #[test]
    fn e2e_input_forwarder_bracketed_paste() {
        let mut buffer = Vec::new();
        {
            let mut fwd = InputForwarder::new(&mut buffer);
            fwd.set_bracketed_paste(true);
            fwd.forward_paste("hello\nworld").unwrap();
        }

        let expected = [BracketedPaste::START, b"hello\nworld", BracketedPaste::END].concat();
        assert_eq!(buffer, expected);

        log_jsonl("input_forwarder", "paste_bracketed", true, "");
    }

    #[test]
    fn e2e_special_keys() {
        // Tab → 0x09
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::Tab)), vec![0x09]);
        // Shift+Tab → CSI Z (backtab)
        assert_eq!(
            key_to_sequence(KeyEvent::new(Key::Tab, Modifiers::SHIFT)),
            b"\x1b[Z"
        );
        // Escape → 0x1b
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::Escape)), vec![0x1b]);
        // Backspace → DEL (0x7f)
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::Backspace)), vec![0x7f]);
        // Insert → CSI 2 ~
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::Insert)), b"\x1b[2~");
        // Delete → CSI 3 ~
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::Delete)), b"\x1b[3~");
        // PageUp → CSI 5 ~
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::PageUp)), b"\x1b[5~");
        // PageDown → CSI 6 ~
        assert_eq!(key_to_sequence(KeyEvent::plain(Key::PageDown)), b"\x1b[6~");

        log_jsonl("input_special", "all_keys", true, "");
    }

    #[test]
    fn e2e_utf8_input() {
        // UTF-8 character via key_to_sequence
        let seq = key_to_sequence(KeyEvent::plain(Key::Char('日')));
        assert_eq!(std::str::from_utf8(&seq).unwrap(), "日");

        // Note: VirtualTerminal uses placeholder for multi-byte, so we test
        // that the sequence itself is correct UTF-8
        log_jsonl("input_utf8", "encoding", true, "");
    }
}

// =============================================================================
// Scenario 4: Resize Handling & Terminal Size Variations
// =============================================================================

mod resize_handling {
    use super::*;

    #[test]
    fn e2e_various_terminal_sizes() {
        let sizes: [(u16, u16); 5] = [(80, 24), (120, 40), (40, 10), (1, 1), (200, 60)];

        for (w, h) in sizes {
            let vt = VirtualTerminal::new(w, h);
            assert_eq!(vt.width(), w);
            assert_eq!(vt.height(), h);
            assert_eq!(vt.cursor(), (0, 0));
        }

        log_jsonl("resize", "various_sizes", true, "5 sizes tested");
    }

    #[test]
    fn e2e_content_reflow_on_different_widths() {
        // Same content, different widths — wrapping behavior changes
        let content = b"ABCDEFGHIJ"; // 10 chars

        // Width 10: single line
        let mut vt10 = VirtualTerminal::new(10, 3);
        vt10.feed(content);
        assert_eq!(vt10.row_text(0), "ABCDEFGHIJ");
        assert_eq!(vt10.row_text(1), "");

        // Width 5: wraps to 2 lines
        let mut vt5 = VirtualTerminal::new(5, 3);
        vt5.feed(content);
        assert_eq!(vt5.row_text(0), "ABCDE");
        assert_eq!(vt5.row_text(1), "FGHIJ");

        // Width 3: wraps to 4 lines (with scroll in 3-row terminal)
        let mut vt3 = VirtualTerminal::new(3, 3);
        vt3.feed(content);
        // 10 chars / 3 = 4 lines (ABC, DEF, GHI, J)
        // Only last 3 visible, first scrolled
        assert!(vt3.scrollback_len() >= 1);

        log_jsonl("resize", "content_reflow", true, "");
    }

    #[test]
    fn e2e_cursor_clamped_to_bounds() {
        let mut vt = VirtualTerminal::new(10, 5);

        // Try to move cursor way out of bounds
        vt.feed(b"\x1b[999;999H");
        assert_eq!(vt.cursor(), (9, 4), "Cursor must clamp to (w-1, h-1)");

        // Try extreme up movement
        vt.feed(b"\x1b[999A");
        assert_eq!(vt.cursor(), (9, 0), "Cursor must clamp at top");

        // Try extreme left movement
        vt.feed(b"\x1b[999D");
        assert_eq!(vt.cursor(), (0, 0), "Cursor must clamp at left");

        log_jsonl("resize", "cursor_clamp", true, "");
    }

    #[test]
    fn e2e_minimum_terminal_size() {
        // 1x1 terminal should work without panicking
        let mut vt = VirtualTerminal::new(1, 1);
        vt.feed(b"X");
        assert_eq!(vt.char_at(0, 0), Some('X'));

        // Write more — should wrap/scroll without panic
        vt.feed(b"YZ");
        assert_eq!(vt.char_at(0, 0), Some('Z'));
        assert!(vt.scrollback_len() >= 1);

        log_jsonl("resize", "minimum_1x1", true, "");
    }

    #[test]
    fn e2e_rapid_size_content_changes() {
        // Simulate rapid content at various sizes (stress test)
        let sizes: [(u16, u16); 4] = [(80, 24), (40, 10), (120, 40), (5, 3)];

        for (w, h) in sizes {
            let mut vt = VirtualTerminal::new(w, h);

            // Feed a variety of content and sequences
            vt.feed(b"Hello World");
            vt.feed(b"\x1b[H"); // Home
            vt.feed(b"\x1b[2J"); // Clear screen
            vt.feed(b"\x1b[38;2;255;0;0mRED\x1b[0m"); // Red text
            vt.feed(b"\r\nLine2\r\nLine3");
            vt.feed(b"\x1b[1;1H"); // Back to origin
            vt.feed(b"Overwrite");

            // Should not panic and should produce valid state
            assert!(vt.cursor().0 < w);
            assert!(vt.cursor().1 < h);
        }

        log_jsonl("resize", "rapid_changes", true, "4 sizes");
    }

    #[test]
    fn e2e_quirks_at_various_sizes() {
        let quirk_sets = [
            ("empty", QuirkSet::empty()),
            ("tmux", QuirkSet::tmux_nested()),
            ("screen", QuirkSet::gnu_screen()),
            ("windows", QuirkSet::windows_console()),
        ];

        for (label, quirks) in quirk_sets {
            let mut vt = VirtualTerminal::with_quirks(20, 5, quirks);
            vt.feed(b"Testing quirks");
            vt.feed(b"\x1b[?1049h"); // Alt screen
            vt.feed(b"Alt");
            vt.feed(b"\x1b[?1049l"); // Exit alt screen

            // Should not panic regardless of quirk
            let text = vt.screen_text();
            assert!(
                !text.is_empty() || quirks == QuirkSet::empty(),
                "Quirk {label} should produce valid output"
            );
        }

        log_jsonl("resize", "quirks", true, "4 quirk sets");
    }

    #[test]
    fn e2e_screen_text_output() {
        let mut vt = VirtualTerminal::new(20, 3);
        vt.feed(b"First\r\nSecond\r\nThird");

        let text = vt.screen_text();
        assert_eq!(text, "First\nSecond\nThird");

        log_jsonl("resize", "screen_text", true, "");
    }

    #[test]
    fn e2e_cpr_da1_responses() {
        let mut vt = VirtualTerminal::new(80, 24);
        vt.feed(b"\x1b[5;10H");

        let cpr = vt.cpr_response();
        assert_eq!(cpr, b"\x1b[5;10R", "CPR response 1-indexed");

        let da1 = vt.da1_response();
        assert_eq!(da1, b"\x1b[?62;22c", "DA1 response VT220");

        log_jsonl("resize", "query_responses", true, "");
    }
}

// =============================================================================
// Scenario 5: Mouse Navigation via PTY
// =============================================================================

#[cfg(unix)]
mod mouse_navigation {
    use super::*;
    use ftui_pty::{PtyConfig, spawn_command};
    use portable_pty::CommandBuilder;

    fn sgr_mouse_sequence(button: u16, x: u16, y: u16, press: bool) -> Vec<u8> {
        let x = x.saturating_add(1);
        let y = y.saturating_add(1);
        let suffix = if press { 'M' } else { 'm' };
        format!("\x1b[<{};{};{}{}", button, x, y, suffix).into_bytes()
    }

    fn find_mouse_event<'a>(output: &'a str, action: &str) -> Option<&'a str> {
        let needle = format!("\"action\":\"{action}\"");
        output.lines().find(|line| {
            line.contains("\"event\":\"mouse_event\"") && line.contains(needle.as_str())
        })
    }

    #[test]
    fn e2e_mouse_tab_switches_screen() -> Result<(), String> {
        let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
            format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
        })?;

        let config = PtyConfig::default()
            .with_size(120, 40)
            .with_test_name("mouse_tab_switch")
            .with_env("E2E_JSONL", "1")
            .with_env("FTUI_DEMO_EXIT_AFTER_MS", "1800")
            .logging(false);

        let mut cmd = CommandBuilder::new(demo_bin);
        cmd.arg("--screen=2");

        let mut session =
            spawn_command(config, cmd).map_err(|err| format!("spawn demo in PTY: {err}"))?;
        std::thread::sleep(Duration::from_millis(250));
        let _ = session.read_output_result();

        let down = sgr_mouse_sequence(0, 1, 0, true);
        session
            .send_input(&down)
            .map_err(|err| format!("send mouse down: {err}"))?;
        std::thread::sleep(Duration::from_millis(60));
        let _ = session.read_output_result();

        let up = sgr_mouse_sequence(0, 1, 0, false);
        session
            .send_input(&up)
            .map_err(|err| format!("send mouse up: {err}"))?;
        std::thread::sleep(Duration::from_millis(120));
        let _ = session.read_output_result();

        let quit = key_to_sequence(KeyEvent::new(Key::Char('q'), Modifiers::NONE));
        let _ = session.send_input(&quit);

        let result = session.wait_and_drain(Duration::from_secs(6));
        let output = session.output().to_vec();
        match result {
            Ok(status) if status.success() => {
                let text = String::from_utf8_lossy(&output);
                let Some(line) = find_mouse_event(&text, "switch_screen") else {
                    let tail = text
                        .chars()
                        .rev()
                        .take(2048)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>();
                    return Err(format!(
                        "mouse_event switch_screen not found in PTY output\nTAIL:\n{tail}"
                    ));
                };
                let has_target = line.contains("\"target_screen\":\"Guided Tour\"");
                log_jsonl(
                    "mouse_tab",
                    "switch_screen",
                    has_target,
                    "target=Guided Tour",
                );
                if !has_target {
                    return Err(format!("mouse_event did not target Guided Tour: {line}"));
                }
                Ok(())
            }
            Ok(status) => {
                let tail = String::from_utf8_lossy(&output);
                Err(format!(
                    "PTY exit status failure: {status:?}\nTAIL:\n{tail}"
                ))
            }
            Err(err) => {
                let tail = String::from_utf8_lossy(&output);
                Err(format!("PTY wait_and_drain error: {err}\nTAIL:\n{tail}"))
            }
        }
    }
}
