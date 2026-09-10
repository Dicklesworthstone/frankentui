#![forbid(unsafe_code)]

//! High-volume generated streaming demo with an optional real child command.
//!
//! Run: `cargo run -p ftui-harness --example streaming`
//! Child: `cargo run -p ftui-harness --example streaming -- --exit-when-child-exits -- seq 1 10000`
//! Interactive: `cargo run -p ftui-harness --example streaming -- --stdin -- cat`
//!
//! This executable keeps the demo's fifteen-row default and opt-in child stdin.
//! It assembles the same public model as the canonical `ftui` agent shell.
//! Child logs default to sanitization; `--log-mode=sgr-only` preserves SGR,
//! while `--log-mode=raw` trusts terminal commands. Generated logs retain SGR.
//! `--ui-height=N`, `--exit-when-child-exits`, and `--exit-after-ms=N` keep their
//! existing behavior. CLI settings override the corresponding
//! `FTUI_AGENT_SHELL_LOG_MODE` and `FTUI_AGENT_SHELL_EXIT_AFTER_MS` defaults.
//! Pending child prompts appear as sanitized previews; completed lines alone
//! enter logs and counters. Ctrl-C interrupts the immediate child, a second
//! press within two seconds quits, and F5 restarts after confirmed cleanup.
//! This does not provide descendant-tree cleanup or binary output streaming.

use ftui::agent_shell::{CHILD_LINK_RETENTION_LINES, StreamingHarness, StreamingOptions};
use ftui::{App, ScreenMode};

fn main() -> std::io::Result<()> {
    let options = StreamingOptions::parse(
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
