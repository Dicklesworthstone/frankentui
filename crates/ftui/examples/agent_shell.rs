#![forbid(unsafe_code)]

//! Inline child session through the public `ftui` API and default features.
//!
//! Run: `cargo run -p ftui --example agent_shell -- -- cat`
//! Finite output: `cargo run -p ftui --example agent_shell -- --exit-when-child-exits -- seq 1 10000`
//!
//! A command after `--` is required. Defaults are three rows, enabled child
//! stdin, sanitized logs, and staying open after exit until Q, Esc, or Ctrl-C.
//! Enter queues a line; Ctrl-D closes stdin; Ctrl-C interrupts the immediate
//! child, and a second press within two seconds quits. F5 starts a fresh run
//! after final output and confirmed child cleanup. Active input treats Q as text.
//!
//! `--pty`, `--log-mode=sanitized|sgr-only|raw`, `--ui-height=N`,
//! `--exit-when-child-exits`, and `--exit-after-ms=N` select session behavior.
//! `--pty` gives the child a pseudo-terminal and its own process group, which
//! Ctrl-C interrupts and quitting kills.
//! CLI log mode and deadline override `FTUI_AGENT_SHELL_LOG_MODE` and
//! `FTUI_AGENT_SHELL_EXIT_AFTER_MS`. Raw mode trusts child terminal commands.
//! Pending prompts are sanitized previews; only completed lines enter logs.
//! Three-row input remains usable without a border consuming its only row.
//! `[stdin queued]` reports admission, not acknowledgment by the child.
//! Without `--pty`, process-tree cleanup is not supplied; binary streaming and
//! physical hyperlink activation never are. Named example tracing is not installed.

use ftui::agent_shell::{CHILD_LINK_RETENTION_LINES, StreamingHarness, StreamingOptions};
use ftui::{App, ScreenMode};

fn main() -> std::io::Result<()> {
    let options = StreamingOptions::parse_agent_shell(
        std::env::args().skip(1),
        std::env::var_os("FTUI_AGENT_SHELL_LOG_MODE"),
        std::env::var_os("FTUI_AGENT_SHELL_EXIT_AFTER_MS"),
    )?;
    let ui_height = options.ui_height();
    App::new(StreamingHarness::new(options))
        .screen_mode(ScreenMode::Inline { ui_height })
        .with_hyperlink_limit(CHILD_LINK_RETENTION_LINES)
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui::agent_shell::Msg;
    use ftui::render::link_registry::LinkRegistry;
    use ftui::render::sanitize::SanitizeMode;
    use ftui::runtime::ProcessEvent;
    use ftui::{Cmd, Event, Frame, GraphemePool, KeyCode, KeyEvent, Model, Modifiers};

    fn model_with_args<const N: usize>(args: [&str; N]) -> StreamingHarness {
        let options =
            StreamingOptions::parse_agent_shell(args.map(str::to_owned), None, None).unwrap();
        StreamingHarness::new(options)
    }

    fn proc_msg(model: &StreamingHarness, event: ProcessEvent) -> Msg {
        Msg::Process {
            generation: model.current_generation().unwrap(),
            event,
        }
    }

    #[test]
    fn canonical_defaults_render_three_rows_with_active_input_through_ftui() {
        let options =
            StreamingOptions::parse_agent_shell(["--", "cat"].map(str::to_owned), None, None)
                .unwrap();
        assert_eq!(options.ui_height(), 3);
        let mut model = StreamingHarness::new(options);
        assert!(matches!(
            model.update(Event::Key(KeyEvent::new(KeyCode::Char('q'))).into()),
            ftui::Cmd::None
        ));
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 3, &mut pool);
        model.view(&mut frame);
        let row = |y| {
            frame
                .buffer
                .row_cells(y)
                .iter()
                .map(|cell| cell.content.as_char().unwrap_or(' '))
                .collect::<String>()
        };
        assert!(row(0).contains("sanitized"));
        assert!(row(1).starts_with("> q"));
        assert!(row(2).starts_with("Enter sends;"));
    }

    #[test]
    fn canonical_requires_child_before_terminal_assembly() {
        for arguments in [vec![], vec!["--"], vec!["cat"], vec!["--ui-height=3"]] {
            let error = StreamingOptions::parse_agent_shell(
                arguments.into_iter().map(str::to_owned),
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(!error.to_string().contains('\x1b'));
        }
    }

    #[test]
    fn stdout_line_increments_counters() {
        let mut model = model_with_args(["--", "cat"]);
        let cmd = model.update(proc_msg(&model, ProcessEvent::Stdout("abc".into())));
        assert_eq!(model.line_count(), 1);
        assert_eq!(model.byte_count(), 4);
        assert!(
            matches!(cmd, Cmd::Log { ref text, mode: SanitizeMode::Strip } if text == "abc"),
            "expected sanitized Cmd::Log(\"abc\"), got {cmd:?}"
        );
    }

    #[test]
    fn stderr_line_is_tagged_in_sanitized_mode() {
        let mut model = model_with_args(["--", "cat"]);
        let cmd = model.update(proc_msg(&model, ProcessEvent::Stderr("warn".into())));
        assert_eq!(model.stderr_count(), 1);
        assert!(
            matches!(cmd, Cmd::Log { ref text, mode: SanitizeMode::Strip } if text == "[stderr] warn"),
            "expected sanitized Cmd::Log(\"[stderr] warn\"), got {cmd:?}"
        );
    }

    #[test]
    fn exit_event_sets_state_and_status_text() {
        let mut model = model_with_args(["--", "cat"]);
        let _ = model.update(proc_msg(&model, ProcessEvent::Exited(3)));
        assert!(model.child_finished());
        assert_eq!(model.child_status(), "EXIT 3");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 3, &mut pool);
        model.view(&mut frame);
        let row0: String = frame
            .buffer
            .row_cells(0)
            .iter()
            .map(|c| c.content.as_char().unwrap_or(' '))
            .collect();
        assert!(
            row0.to_ascii_lowercase().contains("exit 3"),
            "status bar must show exit 3; got: {row0:?}"
        );
    }

    #[test]
    fn ctrl_c_first_forwards_second_quits() {
        let mut model = model_with_args(["--", "cat"]);
        let ctrl_c = || Msg::Key(KeyEvent::new(KeyCode::Char('c')).with_modifiers(Modifiers::CTRL));
        let first = model.update(ctrl_c());
        assert!(
            !matches!(first, Cmd::Quit),
            "first Ctrl-C while running must request interrupt and arm quit, not quit immediately"
        );
        let second = model.update(ctrl_c());
        assert!(
            matches!(second, Cmd::Quit),
            "second Ctrl-C within 2 s must return Cmd::Quit, got {second:?}"
        );
    }

    #[test]
    fn q_quits_only_after_child_exited() {
        let mut model = model_with_args(["--", "cat"]);
        let q_msg = || Msg::Key(KeyEvent::new(KeyCode::Char('q')));
        let while_running = model.update(q_msg());
        assert!(matches!(while_running, Cmd::None));
        assert_eq!(model.input_value(), "q");

        let _ = model.update(proc_msg(&model, ProcessEvent::Exited(0)));
        let after_exit = model.update(q_msg());
        assert!(matches!(after_exit, Cmd::Quit));
    }

    #[test]
    fn enter_sends_input_line_and_echoes() {
        let mut model = model_with_args(["--", "cat"]);
        for ch in ['l', 's'] {
            let _ = model.update(Msg::Key(KeyEvent::new(KeyCode::Char(ch))));
        }
        assert_eq!(model.input_value(), "ls");
        let cmd = model.update(Msg::Key(KeyEvent::new(KeyCode::Enter)));
        assert_eq!(model.input_value(), "");
        assert!(
            matches!(cmd, Cmd::Log { ref text, mode: SanitizeMode::Strip } if text.contains("ls")),
            "expected sanitized input echo containing 'ls', got {cmd:?}"
        );
    }

    #[test]
    fn log_mode_wraps_correctly() {
        let mut sgr_model = model_with_args(["--log-mode=sgr-only", "--", "cat"]);
        let sgr_cmd = sgr_model.update(proc_msg(&sgr_model, ProcessEvent::Stdout("hi".into())));
        assert!(matches!(
            sgr_cmd,
            Cmd::Log {
                text,
                mode: SanitizeMode::SgrOnly
            } if text == "hi"
        ));

        let mut raw_model = model_with_args(["--log-mode=raw", "--", "cat"]);
        let raw_cmd = raw_model.update(proc_msg(&raw_model, ProcessEvent::Stdout("hi".into())));
        assert!(matches!(
            raw_cmd,
            Cmd::Log {
                text,
                mode: SanitizeMode::Raw
            } if text == "hi"
        ));
    }

    #[test]
    fn links_extracted_from_recent_lines() {
        let mut model = model_with_args(["--ui-height=15", "--", "cat"]);
        let _ = model.update(proc_msg(
            &model,
            ProcessEvent::Stdout("see https://example.com/x for details".into()),
        ));
        let mut pool = GraphemePool::new();
        let mut links = LinkRegistry::new();
        let mut frame = Frame::with_links(80, 15, &mut pool, &mut links);
        model.view(&mut frame);
        assert_eq!(links.len(), 1);
        assert_eq!(links.get(1), Some("https://example.com/x"));
    }
}
