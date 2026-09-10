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
//! `--log-mode=sanitized|sgr-only|raw`, `--ui-height=N`,
//! `--exit-when-child-exits`, and `--exit-after-ms=N` select session behavior.
//! CLI log mode and deadline override `FTUI_AGENT_SHELL_LOG_MODE` and
//! `FTUI_AGENT_SHELL_EXIT_AFTER_MS`. Raw mode trusts child terminal commands.
//! Pending prompts are sanitized previews; only completed lines enter logs.
//! Three-row input remains usable without a border consuming its only row.
//! `[stdin queued]` reports admission, not acknowledgment by the child.
//! Process-tree cleanup, binary streaming, and physical hyperlink activation
//! are not supplied by this example. Named example tracing is not installed.

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
    use ftui::{Event, Frame, GraphemePool, KeyCode, KeyEvent, Model};

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
}
