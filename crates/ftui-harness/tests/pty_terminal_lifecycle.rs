#![forbid(unsafe_code)]
#![cfg(unix)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ftui_core::terminal_session::SessionOptions;
use ftui_harness::ADVERSARIAL_PAYLOADS;
use ftui_pty::virtual_terminal::VirtualTerminal;
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
        intercept_signals: true,
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
        intercept_signals: true,
    };
    let expectations = CleanupExpectations::for_session(&options);
    assert_terminal_restored(&output, &expectations)
        .expect("alt-screen terminal cleanup verification failed");
}

fn run_log_injection(mode: &str, strategy: &str, anchor: &str) -> Vec<u8> {
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ftui-harness"));
    cmd.env("FTUI_HARNESS_LOG_INJECTION", mode);
    cmd.env("FTUI_HARNESS_LOG_STRATEGY", strategy);
    cmd.env("FTUI_HARNESS_LOG_ANCHOR", anchor);
    let mut session = spawn_command(
        PtyConfig::default()
            .with_size(80, 24)
            .with_test_name(format!("log_injection_{mode}_{strategy}_{anchor}")),
        cmd,
    )
    .expect("spawn real log writer in PTY");
    let timeout = Duration::from_secs(10);
    session
        .read_until(b"LOG_BASELINE", timeout)
        .expect("baseline handshake");
    let baseline = session
        .master()
        .get_termios()
        .expect("PTY termios supported");
    session.send_input(b"go\n").expect("acknowledge baseline");
    session
        .read_until(b"LOG_ACTIVE", timeout)
        .expect("raw-mode handshake");
    let active = session.master().get_termios().expect("read active termios");
    assert_ne!(
        active.local_flags, baseline.local_flags,
        "session must actually enter raw mode"
    );
    session
        .send_input(b"g")
        .expect("acknowledge active session");
    let output = session
        .read_until(b"LOG_RESTORED", timeout)
        .expect("cleanup handshake");
    // Save evidence before the state assertions, including for a failed check.
    let end = find_sequence(&output, b"LOG_RESTORED").expect("restored marker");
    let captured = &output[..end];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/pty_injection");
    std::fs::create_dir_all(&root).expect("create PTY capture directory");
    // Captures survive repeated runs; a recycled PID must not collide with a
    // previous run's artifacts (which create_new deliberately never replaces).
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("artifact timestamp")
        .as_nanos();
    let name = format!(
        "{}-{timestamp}-{mode}-{strategy}-{anchor}",
        std::process::id()
    );
    let mut model = VirtualTerminal::new(80, 24);
    model.feed(captured);
    let state = serde_json::json!({
        "mode": mode, "strategy": strategy, "anchor": anchor,
        "screen_text": model.screen_text(), "cursor": model.cursor(),
        "cursor_visible": model.cursor_visible(), "title": model.title(),
        "scroll_region": model.scroll_region(), "alt_screen": model.is_alternate_screen(),
        "origin_mode": model.origin_mode(), "insert_mode": model.insert_mode(),
        "autowrap": model.autowrap(),
    });
    for (extension, bytes) in [
        ("raw", captured.to_vec()),
        (
            "model.json",
            serde_json::to_vec_pretty(&state).expect("serialize terminal state"),
        ),
    ] {
        use std::io::Write;
        let path = root.join(format!("{name}.{extension}"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("create unique PTY artifact");
        file.write_all(&bytes).expect("write PTY artifact");
        eprintln!("PTY_LOG_ARTIFACT {}", path.display());
    }
    let restored = session
        .master()
        .get_termios()
        .expect("read restored termios");
    assert_eq!(
        restored, baseline,
        "PTY attributes must be restored exactly"
    );
    session.send_input(b"done\n").expect("acknowledge cleanup");
    assert!(
        session
            .wait_and_drain(timeout)
            .expect("drain child")
            .success()
    );
    // Exclude the reporting text after cleanup, which would overwrite cells.
    captured.to_vec()
}

fn assert_no_log_injection(bytes: &[u8]) {
    for forbidden in [
        b"\x1b[2J".as_slice(),
        b"\x1b[3J",
        b"\x1b[?1049h",
        b"\x1b[?47h",
        b"\x1b]",
        b"\x1bP",
        b"\x1b_",
        b"\x1b^",
        b"\x1bX",
        b"\x1bc",
        b"\x1b[?6h",
        b"\x1b[?7l",
        b"\x1b[4h",
        b"\x1b[2;10r",
    ] {
        assert!(
            find_sequence(bytes, forbidden).is_none(),
            "injected sequence {forbidden:?} survived; capture={bytes:?}"
        );
    }
    assert!(
        !String::from_utf8_lossy(bytes)
            .chars()
            .any(|c| ('\u{80}'..='\u{9f}').contains(&c))
    );
}

#[test]
fn pty_log_injection_preserves_text_style_and_terminal_state() {
    let expected: String = ADVERSARIAL_PAYLOADS.iter().map(|&(_, text)| text).collect();
    for strategy in ["scroll", "overlay"] {
        for anchor in ["top", "bottom"] {
            let control_bytes = run_log_injection("control", strategy, anchor);
            let mut control = VirtualTerminal::new(80, 24);
            control.feed(&control_bytes);
            assert!(
                control.screen_text().contains(&expected),
                "control must render the whole expected log"
            );
            assert!(
                control.screen_text().contains("CHROME"),
                "control must retain UI chrome"
            );
            for mode in ["strip", "sgr"] {
                let output = run_log_injection(mode, strategy, anchor);
                assert_no_log_injection(&output);
                let mut actual = VirtualTerminal::new(80, 24);
                actual.feed(&output);
                assert_eq!(
                    actual.screen_text(),
                    control.screen_text(),
                    "{mode}/{strategy}/{anchor}; capture={output:?}"
                );
                assert_eq!(actual.cursor(), control.cursor());
                assert_eq!(actual.title(), control.title());
                assert_eq!(actual.scroll_region(), (0, 23));
                assert_eq!(actual.origin_mode(), control.origin_mode());
                assert_eq!(actual.insert_mode(), control.insert_mode());
                assert_eq!(actual.autowrap(), control.autowrap());
                assert!(!actual.is_alternate_screen());
                assert!(actual.cursor_visible());
                let mut colored_cells = 0;
                for y in 0..24 {
                    for x in 0..80 {
                        if actual.style_at(x, y) != control.style_at(x, y) {
                            assert_eq!(mode, "sgr", "strip mode must preserve no injected style");
                            assert_eq!(actual.char_at(x, y), Some('C'));
                            let color = actual.style_at(x, y).expect("cell in bounds").fg;
                            assert_eq!(
                                color,
                                Some(ftui_pty::virtual_terminal::Color { r: 255, g: 0, b: 0 })
                            );
                            colored_cells += 1;
                        }
                    }
                }
                assert_eq!(colored_cells, usize::from(mode == "sgr"));
            }
        }
    }
}

#[test]
fn pty_log_raw_requires_explicit_opt_in_and_restores_termios() {
    for strategy in ["scroll", "overlay"] {
        let output = run_log_injection("raw", strategy, "bottom");
        // The same planted attack must be observable in Raw, so this test
        // cannot pass if the harness always selects the sanitized branch.
        assert!(find_sequence(&output, b"\x1b[2J").is_some());
        assert!(find_sequence(&output, b"\x1b]52;c;aGk=\x07").is_some());
        assert!(find_sequence(&output, b"\x1b[?25h").is_some());
        // Arbitrary Raw commands may corrupt terminal modes; only the actual
        // kernel termios restoration checked by run_log_injection is promised.
    }
}

#[test]
fn raw_log_emits_mode_and_byte_count_trace() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{Layer, layer::SubscriberExt};

    #[derive(Default, Debug, PartialEq)]
    struct Fields {
        mode: String,
        bytes: u64,
    }
    impl tracing::field::Visit for Fields {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "mode" {
                self.mode = value.to_owned();
            }
        }
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            if field.name() == "bytes" {
                self.bytes = value;
            }
        }
        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }
    struct Capture(Arc<Mutex<Vec<Fields>>>);
    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == "ftui.runtime.log" {
                let mut fields = Fields::default();
                event.record(&mut fields);
                self.0.lock().expect("capture lock").push(fields);
            }
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(Capture(Arc::clone(&events)));
    tracing::subscriber::with_default(subscriber, || {
        let mut writer = ftui_runtime::TerminalWriter::new(
            Vec::new(),
            ftui_runtime::ScreenMode::Inline { ui_height: 3 },
            ftui_runtime::UiAnchor::Bottom,
            ftui_core::terminal_capabilities::TerminalCapabilities::basic(),
        );
        writer.write_log("plain").expect("strip");
        writer.write_log_sgr_only("\x1b[31mred").expect("SGR");
        writer.write_log_raw("\x1b[2J界").expect("raw");
    });
    assert_eq!(
        *events.lock().expect("capture lock"),
        vec![Fields {
            mode: "raw".into(),
            bytes: 7
        }]
    );
}
