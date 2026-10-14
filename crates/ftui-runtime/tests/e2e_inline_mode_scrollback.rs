#![forbid(unsafe_code)]

//! bd-1q5.17 / G48 (`bd-g00-root-epic-ewths.47.2`): inline mode keeps log
//! output in real scrollback.
//!
//! Every assertion here is made against what a terminal would show, not
//! against the raw byte stream: the writer's exact output is fed into an
//! independent VT emulator (the `vt100` crate) and the test reads that
//! emulator's scrollback and screen. A byte-level check ("the log text
//! appears somewhere in the output") passes even when each log line
//! overwrites the previous one on a single row, which is how the overlay
//! strategy lost every line inside tmux while these tests stayed green.
//!
//! Covers:
//! 1. Log lines reach history exactly once, in order, around UI presents
//! 2. Logs written before the first frame, long wrapped lines, wide text
//! 3. The first frame does not overwrite the shell's last lines
//! 4. Inline-auto growth does not overwrite the newest log rows
//! 5. No UI row ever enters history while the writer is live
//! 6. Teardown leaves the final frame and parks the cursor on a fresh line
//! 7. The DECSTBM path keeps the UI intact (visible-screen check only)
//!
//! Run:
//!   cargo test -p ftui-runtime --test e2e_inline_mode_scrollback

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_runtime::{ScreenMode, TerminalWriter, UiAnchor, inline_active_widgets};

// ============================================================================
// Helpers
// ============================================================================

/// Overlay strategy: no DECSTBM support advertised (also what every
/// multiplexer identity and a failed DECSTBM self-test select).
fn basic_caps() -> TerminalCapabilities {
    TerminalCapabilities::basic()
}

/// Sync output but still no DECSTBM: overlay with DEC 2026 brackets.
fn full_caps() -> TerminalCapabilities {
    let mut caps = TerminalCapabilities::basic();
    caps.color_depth = ColorDepth::TrueColor;
    caps.sync_output = true;
    caps
}

/// DECSTBM scroll region + sync output: the `ScrollRegion` strategy.
fn scroll_region_caps() -> TerminalCapabilities {
    let mut caps = full_caps();
    caps.scroll_region = true;
    caps
}

/// Check if a byte sequence contains a sub-sequence.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// CSI ?1049h — alternate screen enter (must NOT appear in inline mode).
const ALTSCREEN_ENTER: &[u8] = b"\x1b[?1049h";
/// CSI ?1049l — alternate screen exit (must NOT appear in inline mode).
const ALTSCREEN_EXIT: &[u8] = b"\x1b[?1049l";

/// Output sink a test can read while the writer is still alive, so the
/// live state can be checked before drop-time teardown adds its own bytes.
#[derive(Clone, Default)]
struct SharedOutput(Arc<Mutex<Vec<u8>>>);

impl SharedOutput {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().expect("output lock").clone()
    }

    fn len(&self) -> usize {
        self.0.lock().expect("output lock").len()
    }
}

impl Write for SharedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("output lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Scrollback oracle: an independent VT emulator fed the writer's bytes.
///
/// `vt100` keeps a row in scrollback only when it leaves the top of a
/// full-screen scroll (no DECSTBM region active), which every terminal does
/// on the main screen. xterm, tmux and Alacritty additionally keep rows that
/// scroll out of a region whose top margin is row 1; `vt100` drops them, so
/// a `ScrollRegion` run can only be checked for its visible screen here.
struct Emulator {
    parser: vt100::Parser,
    cols: u16,
    rows: u16,
}

impl Emulator {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 1_000_000),
            cols,
            rows,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        self.cols = cols;
        self.rows = rows;
    }

    /// Rows that left the top of the screen, oldest first.
    fn history(&mut self) -> Vec<String> {
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let mut offset = self.parser.screen().scrollback();
        let mut lines = Vec::with_capacity(offset);
        while offset > 0 {
            self.parser.screen_mut().set_scrollback(offset);
            let take = offset.min(usize::from(self.rows));
            lines.extend(
                self.parser
                    .screen()
                    .rows(0, self.cols)
                    .take(take)
                    .map(|row| row.trim_end().to_string()),
            );
            offset -= take;
        }
        self.parser.screen_mut().set_scrollback(0);
        lines
    }

    /// Visible rows, top to bottom.
    fn visible(&self) -> Vec<String> {
        self.parser
            .screen()
            .rows(0, self.cols)
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    /// History followed by the visible screen: everything a user can scroll
    /// back through.
    fn transcript(&mut self) -> Vec<String> {
        let mut all = self.history();
        all.extend(self.visible());
        all
    }

    fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }
}

/// Assert that the rows starting with `prefix` are exactly `expected`, in
/// order, each once; report the first divergence with context.
fn assert_lines_in_order(rows: &[String], prefix: &str, expected: &[String]) {
    let got: Vec<&String> = rows.iter().filter(|r| r.starts_with(prefix)).collect();
    let first_bad = got
        .iter()
        .zip(expected)
        .position(|(g, e)| g.as_str() != e.as_str());
    if got.len() != expected.len() || first_bad.is_some() {
        let idx = first_bad.unwrap_or(got.len().min(expected.len()));
        panic!(
            "log rows diverge at index {idx}: got {} rows with prefix {prefix:?}, expected {}\n\
             expected[{idx}] = {:?}\n got[{idx}] = {:?}\n first 5 got: {:?}",
            got.len(),
            expected.len(),
            expected.get(idx),
            got.get(idx),
            got.iter().take(5).collect::<Vec<_>>(),
        );
    }
}

fn numbered(prefix: &str, range: std::ops::RangeInclusive<usize>) -> Vec<String> {
    range.map(|i| format!("{prefix}{i:03}")).collect()
}

/// A UI frame whose every row carries a recognisable marker.
fn ui_frame(width: u16, height: u16, tag: &str) -> Buffer {
    let mut buf = Buffer::new(width, height);
    for y in 0..height {
        let text = format!("UI-{tag}-row{y}");
        for (x, ch) in text.chars().enumerate() {
            if let Ok(x) = u16::try_from(x)
                && x < width
            {
                buf.set_raw(x, y, Cell::from_char(ch));
            }
        }
    }
    buf
}

fn assert_no_ui_rows_in_history(emu: &mut Emulator) {
    let leaked: Vec<String> = emu
        .history()
        .into_iter()
        .filter(|row| row.starts_with("UI-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "UI rows must never enter scrollback while the writer is live: {leaked:?}"
    );
}

// ============================================================================
// 1. Write 100 lines then render inline widget
// ============================================================================

#[test]
fn write_100_lines_then_render_inline_widget() {
    let out = SharedOutput::default();
    {
        let mut writer = TerminalWriter::new(
            out.clone(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        for i in 1..=100 {
            writer
                .write_log(&format!("scrollback line {i:03}\n"))
                .unwrap();
        }

        writer
            .present_ui(&ui_frame(80, 5, "A"), None, true)
            .unwrap();
        writer.flush().unwrap();
    }

    let output = out.bytes();
    let mut emu = Emulator::new(80, 24);
    emu.feed(&output);
    assert_lines_in_order(
        &emu.transcript(),
        "scrollback line ",
        &numbered("scrollback line ", 1..=100),
    );

    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "inline mode must never emit CSI ?1049h"
    );
    assert!(
        !contains_bytes(&output, ALTSCREEN_EXIT),
        "inline mode must never emit CSI ?1049l"
    );
}

// ============================================================================
// 2. Scrollback preserved around the inline render
// ============================================================================

#[test]
fn scrollback_accessible_above_widget() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(80, 24);

    for i in 1..=100 {
        writer.write_log(&format!("log line {i:03}\n")).unwrap();
    }
    writer
        .present_ui(&ui_frame(80, 5, "A"), None, true)
        .unwrap();
    // Written after the render: must also reach history.
    writer.write_log("log line 101\n").unwrap();
    writer.flush().unwrap();

    let mut emu = Emulator::new(80, 24);
    emu.feed(&out.bytes());
    assert_lines_in_order(
        &emu.transcript(),
        "log line ",
        &numbered("log line ", 1..=101),
    );
    assert_no_ui_rows_in_history(&mut emu);

    // The UI is intact in the bottom five rows after the post-render log.
    let visible = emu.visible();
    for y in 0..5 {
        assert_eq!(visible[19 + y], format!("UI-A-row{y}"), "UI row {y}");
    }
    drop(writer);
}

// ============================================================================
// 3. Remove widget, verify scrollback intact; teardown contract
// ============================================================================

#[test]
fn scrollback_intact_after_widget_removal() {
    let out = SharedOutput::default();
    {
        let mut writer = TerminalWriter::new(
            out.clone(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        for i in 1..=100 {
            writer
                .write_log(&format!("preserved line {i:03}\n"))
                .unwrap();
        }
        writer
            .present_ui(&ui_frame(80, 5, "Z"), None, true)
            .unwrap();
        // Logs still pending at drop must not be lost.
        writer.write_log("preserved line 101\n").unwrap();
        // Writer drop == widget removal (RAII cleanup)
    }

    let output = out.bytes();
    let mut emu = Emulator::new(80, 24);
    emu.feed(&output);
    assert_lines_in_order(
        &emu.transcript(),
        "preserved line ",
        &numbered("preserved line ", 1..=101),
    );

    // Teardown contract: the final frame stays, and the cursor sits at the
    // start of an empty row below it, so the shell's next output cannot
    // overwrite part of a UI row.
    let visible = emu.visible();
    let (row, col) = emu.cursor();
    assert_eq!(
        (row, col),
        (23, 0),
        "cursor parked on the last row, column 1"
    );
    assert_eq!(visible[23], "", "the parking row must be empty");
    for y in 0..5 {
        assert_eq!(
            visible[18 + y],
            format!("UI-Z-row{y}"),
            "final frame row {y}"
        );
    }

    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "inline mode must never emit alternate screen enter"
    );
}

// ============================================================================
// 4. Multiple render passes preserve scrollback
// ============================================================================

#[test]
fn multiple_render_passes_preserve_scrollback() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(80, 24);

    // Interleave log writes and UI renders; the UI changes every pass.
    for batch in 0..10 {
        for i in 0..10 {
            let line_num = batch * 10 + i + 1;
            writer.write_log(&format!("line {line_num:03}\n")).unwrap();
        }
        writer
            .present_ui(&ui_frame(80, 5, &format!("B{batch}")), None, true)
            .unwrap();
    }
    writer.flush().unwrap();

    let mut emu = Emulator::new(80, 24);
    emu.feed(&out.bytes());
    assert_lines_in_order(&emu.transcript(), "line ", &numbered("line ", 1..=100));
    assert_no_ui_rows_in_history(&mut emu);
    let visible = emu.visible();
    for y in 0..5 {
        assert_eq!(
            visible[19 + y],
            format!("UI-B9-row{y}"),
            "last frame row {y}"
        );
    }
    drop(writer);
}

// ============================================================================
// 5. Full E2E lifecycle: write → render → more writes → re-render → cleanup
// ============================================================================
//
// Note: Tracing span assertions (inline.render, scrollback_preserved) are
// covered by the unit test `inline_render_emits_tracing_span_fields` in
// terminal_writer.rs and by golden_frame_e2e.rs tracing tests. Integration
// test binaries cannot reliably use `tracing::subscriber::set_default` in
// parallel due to tracing's global callsite interest cache.

#[test]
fn full_e2e_lifecycle_scrollback_preserved() {
    let out = SharedOutput::default();
    {
        let mut writer = TerminalWriter::new(
            out.clone(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        for i in 1..=100 {
            writer.write_log(&format!("scrollback {i:03}\n")).unwrap();
        }
        writer
            .present_ui(&ui_frame(80, 5, "P1"), None, true)
            .unwrap();
        writer.write_log("scrollback 101\n").unwrap();
        writer.write_log("scrollback 102\n").unwrap();
        writer
            .present_ui(&ui_frame(80, 5, "P2"), None, true)
            .unwrap();
    }

    let output = out.bytes();
    let mut emu = Emulator::new(80, 24);
    emu.feed(&output);
    assert_lines_in_order(
        &emu.transcript(),
        "scrollback ",
        &numbered("scrollback ", 1..=102),
    );
    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "must not enter alt screen"
    );
}

// ============================================================================
// 6. Logs written before the first frame, long wrapped lines, wide text
// ============================================================================

#[test]
fn long_and_wide_log_lines_wrap_into_history_in_order() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 4 },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(40, 12);

    // 70 columns of ASCII wraps once at 40; 25 wide CJK characters (50
    // columns) wrap once too.
    let mut expected = Vec::new();
    for i in 0..30 {
        let line = if i % 2 == 0 {
            format!("long{i:02}-{}", "x".repeat(64))
        } else {
            format!("wide{i:02}-{}", "漢".repeat(25))
        };
        writer.write_log(&format!("{line}\n")).unwrap();
        expected.push(line);
        if i % 7 == 6 {
            writer
                .present_ui(&ui_frame(40, 4, "W"), None, true)
                .unwrap();
        }
    }
    writer.flush().unwrap();

    let mut emu = Emulator::new(40, 12);
    emu.feed(&out.bytes());
    // Wrapped rows concatenate back into the original lines.
    let joined: String = emu.transcript().concat();
    let mut cursor = 0;
    for line in &expected {
        let pos = joined[cursor..]
            .find(line.as_str())
            .unwrap_or_else(|| panic!("log line {line:?} missing or out of order"));
        cursor += pos + line.len();
    }
    assert_no_ui_rows_in_history(&mut emu);
    drop(writer);
}

// ============================================================================
// 7. The first frame must not overwrite what the shell printed last
// ============================================================================

#[test]
fn first_frame_does_not_overwrite_shell_output() {
    let mut emu = Emulator::new(80, 24);
    // A full terminal: 30 lines of earlier shell output, cursor at the
    // start of a fresh bottom row (where the program was launched).
    for i in 1..=30 {
        emu.feed(format!("shell {i:03}\r\n").as_bytes());
    }

    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(80, 24);
    writer
        .present_ui(&ui_frame(80, 5, "F"), None, true)
        .unwrap();
    writer.write_log("app 001\n").unwrap();
    writer.flush().unwrap();

    emu.feed(&out.bytes());
    let transcript = emu.transcript();
    assert_lines_in_order(&transcript, "shell ", &numbered("shell ", 1..=30));
    assert_lines_in_order(&transcript, "app ", &numbered("app ", 1..=1));
    drop(writer);
}

// ============================================================================
// 8. InlineAuto mode: logs survive, growth does not overwrite log rows
// ============================================================================

#[test]
fn inline_auto_mode_preserves_scrollback() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::InlineAuto {
            min_height: 3,
            max_height: 10,
        },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(80, 24);

    for i in 1..=50 {
        writer.write_log(&format!("auto line {i:03}\n")).unwrap();
    }
    writer.set_auto_ui_height(3);
    writer
        .present_ui(&ui_frame(80, 3, "S"), None, true)
        .unwrap();
    for i in 51..=100 {
        writer.write_log(&format!("auto line {i:03}\n")).unwrap();
    }
    writer
        .present_ui(&ui_frame(80, 3, "S"), None, true)
        .unwrap();
    // The UI grows by five rows: the rows above it hold the newest log
    // lines, which must move up rather than be drawn over.
    writer.set_auto_ui_height(8);
    writer
        .present_ui(&ui_frame(80, 8, "G"), None, true)
        .unwrap();
    writer.flush().unwrap();

    let mut emu = Emulator::new(80, 24);
    emu.feed(&out.bytes());
    assert_lines_in_order(
        &emu.transcript(),
        "auto line ",
        &numbered("auto line ", 1..=100),
    );
    assert_no_ui_rows_in_history(&mut emu);
    let visible = emu.visible();
    for y in 0..8 {
        assert_eq!(visible[16 + y], format!("UI-G-row{y}"), "grown UI row {y}");
    }
    drop(writer);
}

// ============================================================================
// 9. Inline widget gauge tracks lifecycle
// ============================================================================

#[test]
fn inline_widget_gauge_tracks_lifecycle() {
    // Retry loop to handle contention from parallel tests that may
    // create/drop TerminalWriters concurrently (same pattern as
    // multiple_inline_writers_gauge_tracks_both in terminal_writer.rs).
    for _ in 0..64 {
        let before = inline_active_widgets();

        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        let during = inline_active_widgets();

        // Write and render
        writer.write_log("gauge test\n").unwrap();
        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();

        drop(writer);

        let after = inline_active_widgets();

        // Check uncontended transitions: before → during (+1) → after (back to before)
        if during == before.saturating_add(1) && after == before {
            return;
        }
        std::thread::yield_now();
    }

    panic!("failed to observe uncontended gauge lifecycle transitions after 64 retries");
}

// ============================================================================
// 10. Full caps (sync output) inline mode preserves scrollback
// ============================================================================

#[test]
fn full_caps_inline_mode_preserves_scrollback() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        full_caps(),
    );
    writer.set_size(80, 24);

    for i in 1..=100 {
        writer.write_log(&format!("sync line {i:03}\n")).unwrap();
        if i % 25 == 0 {
            writer
                .present_ui(&ui_frame(80, 5, "Y"), None, true)
                .unwrap();
        }
    }
    writer.flush().unwrap();

    let output = out.bytes();
    let mut emu = Emulator::new(80, 24);
    emu.feed(&output);
    assert_lines_in_order(
        &emu.transcript(),
        "sync line ",
        &numbered("sync line ", 1..=100),
    );
    assert_no_ui_rows_in_history(&mut emu);
    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "inline mode must never emit alternate screen even with full caps"
    );
    // Every DEC 2026 bracket opened is closed.
    let begins = output.windows(8).filter(|w| *w == b"\x1b[?2026h").count();
    let ends = output.windows(8).filter(|w| *w == b"\x1b[?2026l").count();
    assert!(begins > 0, "sync output must be used when advertised");
    assert_eq!(begins, ends, "sync brackets must balance");
    drop(writer);
}

// ============================================================================
// 11. Resize between renders preserves scrollback
// ============================================================================

#[test]
fn resize_between_renders_preserves_scrollback() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(80, 24);

    for i in 1..=50 {
        writer.write_log(&format!("resize line {i:03}\n")).unwrap();
    }
    writer
        .present_ui(&ui_frame(80, 5, "R"), None, true)
        .unwrap();
    writer.flush().unwrap();
    let before_resize = out.len();

    // The terminal grows (no reflow in either case: lines are < 80 cols).
    writer.set_size(120, 40);
    for i in 51..=100 {
        writer.write_log(&format!("resize line {i:03}\n")).unwrap();
    }
    writer
        .present_ui(&ui_frame(120, 5, "R2"), None, true)
        .unwrap();
    writer.flush().unwrap();

    let output = out.bytes();
    let mut emu = Emulator::new(80, 24);
    emu.feed(&output[..before_resize]);
    emu.resize(120, 40);
    emu.feed(&output[before_resize..]);
    assert_lines_in_order(
        &emu.transcript(),
        "resize line ",
        &numbered("resize line ", 1..=100),
    );
    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "resize must not trigger alternate screen"
    );
    drop(writer);
}

// ============================================================================
// 12. into_inner returns writer with scrollback intact
// ============================================================================

#[test]
fn into_inner_preserves_scrollback() {
    let mut writer = TerminalWriter::new(
        Vec::new(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        basic_caps(),
    );
    writer.set_size(80, 24);

    for i in 1..=100 {
        writer.write_log(&format!("inner line {i:03}\n")).unwrap();
    }
    writer
        .present_ui(&ui_frame(80, 5, "I"), None, true)
        .unwrap();
    writer.write_log("inner line 101\n").unwrap();

    // into_inner performs cleanup and returns the underlying writer
    let output = writer.into_inner().expect("into_inner should succeed");
    let mut emu = Emulator::new(80, 24);
    emu.feed(&output);
    assert_lines_in_order(
        &emu.transcript(),
        "inner line ",
        &numbered("inner line ", 1..=101),
    );
    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "into_inner must not emit alternate screen"
    );
}

// ============================================================================
// 13. DECSTBM strategy: UI intact and newest logs visible (see Emulator doc)
// ============================================================================

#[test]
fn scroll_region_strategy_keeps_ui_intact() {
    let out = SharedOutput::default();
    let mut writer = TerminalWriter::new(
        out.clone(),
        ScreenMode::Inline { ui_height: 5 },
        UiAnchor::Bottom,
        scroll_region_caps(),
    );
    writer.set_size(80, 24);
    writer
        .present_ui(&ui_frame(80, 5, "D"), None, true)
        .unwrap();
    assert!(
        writer.scroll_region_active(),
        "ScrollRegion must set DECSTBM"
    );
    for i in 1..=100 {
        writer.write_log(&format!("region line {i:03}\n")).unwrap();
        if i % 20 == 0 {
            writer
                .present_ui(&ui_frame(80, 5, "D"), None, true)
                .unwrap();
        }
    }
    writer.flush().unwrap();

    let mut emu = Emulator::new(80, 24);
    emu.feed(&out.bytes());
    let visible = emu.visible();
    for y in 0..5 {
        assert_eq!(visible[19 + y], format!("UI-D-row{y}"), "UI row {y}");
    }
    // The newest lines fill the region above the UI, in order, ending
    // with the last one.
    let shown: Vec<String> = visible[..19]
        .iter()
        .filter(|row| row.starts_with("region line "))
        .cloned()
        .collect();
    assert!(
        shown.len() >= 17,
        "region shows the newest lines: {shown:?}"
    );
    let first = 100 - shown.len() + 1;
    assert_eq!(shown, numbered("region line ", first..=100));
    drop(writer);
}

// ============================================================================
// 14. Property: random interleavings deliver every line once, in order
// ============================================================================

#[derive(Debug, Clone)]
enum Op {
    /// One `write_log` call with `count` lines of the given shape:
    /// 0 short ASCII, 1 wide CJK, 2 long (wraps), 3 all in one payload.
    Log {
        count: usize,
        shape: u8,
    },
    Present,
    /// Inline-auto only: remeasure to this height.
    Height(u16),
    Flush,
}

fn op_strategy() -> impl proptest::strategy::Strategy<Value = Op> {
    use proptest::prelude::*;
    prop_oneof![
        4 => (1usize..4, 0u8..4).prop_map(|(count, shape)| Op::Log { count, shape }),
        2 => Just(Op::Present),
        1 => (1u16..7).prop_map(Op::Height),
        1 => Just(Op::Flush),
    ]
}

fn log_line(id: usize, shape: u8) -> String {
    match shape {
        1 => format!("L{id:04}-{}", "漢".repeat(5 + id % 23)),
        2 => format!("L{id:04}-{}", "x".repeat(50 + id % 90)),
        _ => format!("L{id:04}-{}", "a".repeat(id % 30)),
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]

    #[test]
    fn overlay_transport_delivers_random_interleavings(
        auto in proptest::prelude::any::<bool>(),
        ops in proptest::collection::vec(op_strategy(), 1..48),
    ) {
        let (cols, rows) = (40u16, 12u16);
        let mode = if auto {
            ScreenMode::InlineAuto { min_height: 1, max_height: 6 }
        } else {
            ScreenMode::Inline { ui_height: 3 }
        };
        let out = SharedOutput::default();
        let mut writer = TerminalWriter::new(out.clone(), mode, UiAnchor::Bottom, basic_caps());
        writer.set_size(cols, rows);
        let mut expected = Vec::new();
        for op in &ops {
            match op {
                Op::Log { count, shape } => {
                    let lines: Vec<String> = (0..*count)
                        .map(|k| log_line(expected.len() + k, *shape % 3))
                        .collect();
                    if *shape == 3 {
                        writer.write_log(&format!("{}\n", lines.join("\n"))).unwrap();
                    } else {
                        for line in &lines {
                            writer.write_log(&format!("{line}\n")).unwrap();
                        }
                    }
                    expected.extend(lines);
                }
                Op::Present => {
                    let height = writer.ui_height();
                    writer.present_ui(&ui_frame(cols, height, "P"), None, true).unwrap();
                }
                Op::Height(h) => {
                    if auto {
                        writer.set_auto_ui_height(*h);
                    }
                }
                Op::Flush => writer.flush().unwrap(),
            }
        }
        writer.flush().unwrap();

        let mut emu = Emulator::new(cols, rows);
        emu.feed(&out.bytes());
        // Wrapped rows concatenate back into each line; search in order.
        let joined: String = emu.transcript().concat();
        let mut cursor = 0;
        for line in &expected {
            let found = joined[cursor..].find(line.as_str());
            proptest::prop_assert!(found.is_some(), "line {line:?} missing or out of order; ops={ops:?}");
            cursor += found.unwrap_or(0) + line.len();
        }
        for (n, line) in expected.iter().enumerate() {
            let id = &line[..5];
            proptest::prop_assert_eq!(joined.matches(id).count(), 1, "line {} not exactly once", n);
        }
        let leaked: Vec<String> =
            emu.history().into_iter().filter(|r| r.starts_with("UI-")).collect();
        proptest::prop_assert!(leaked.is_empty(), "UI rows in history: {leaked:?}; ops={ops:?}");
        drop(writer);
    }
}

// ============================================================================
// 15. Oracle self-check: the emulator distinguishes delivery from overwrite
// ============================================================================

#[test]
fn oracle_detects_single_row_overwrite() {
    // Planted negative: the pre-G48 overlay bytes (save cursor, move to one
    // fixed row, erase it, write the line, restore) keep only the last line.
    let mut lossy = Emulator::new(80, 24);
    for i in 1..=50 {
        lossy.feed(format!("\x1b7\x1b[19;1H\x1b[2Kline {i:03}\x1b8").as_bytes());
    }
    let rows = lossy.transcript();
    let delivered = rows.iter().filter(|r| r.starts_with("line ")).count();
    assert_eq!(delivered, 1, "an overwrite transport delivers one line");

    // Positive control: a plain stream keeps every line.
    let mut plain = Emulator::new(80, 24);
    for i in 1..=50 {
        plain.feed(format!("line {i:03}\r\n").as_bytes());
    }
    assert_lines_in_order(&plain.transcript(), "line ", &numbered("line ", 1..=50));
}

// ============================================================================
// 16. Output written before a resize the writer has not seen yet
// ============================================================================

/// What a multiplexer shows once it has resized a pane to `rows`: the same
/// content kept against the bottom (shrinking pushes top rows into history,
/// growing pulls history rows back down), as tmux does. Built by replaying
/// the transcript into a terminal of the new height.
fn bottom_anchored_resize(emu: &mut Emulator, rows: u16) -> Emulator {
    let mut resized = Emulator::new(emu.cols, rows);
    resized.feed(emu.transcript().join("\r\n").as_bytes());
    resized
}

/// G42 journey, reduced: a terminal reads the writer's bytes after its size
/// changed, before the writer heard of it (bytes in flight, or a coalesced
/// resize). Absolute rows then miss the UI: on a grow, the erase lands on
/// log lines the multiplexer pulled back from history and deletes them; on
/// a shrink, it clamps below the UI, which scrolls into history. Addressing
/// the UI from the last row keeps both cases exact.
#[test]
fn overlay_stream_survives_a_resize_it_has_not_seen_yet() {
    for (rows, case) in [(34, "grown to 34"), (18, "shrunk to 18")] {
        let out = SharedOutput::default();
        let mut writer = TerminalWriter::new(
            out.clone(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        for i in 1..=40 {
            writer.write_log(&format!("pre {i:03}\n")).unwrap();
        }
        writer
            .present_ui(&ui_frame(80, 5, "A"), None, true)
            .unwrap();
        writer.flush().unwrap();
        let mut before = Emulator::new(80, 24);
        before.feed(&out.bytes());

        let mut after = bottom_anchored_resize(&mut before, rows);
        let mark = out.len();
        for i in 1..=10 {
            writer.write_log(&format!("post {i:03}\n")).unwrap();
        }
        writer.flush().unwrap();
        after.feed(&out.bytes()[mark..]);

        let transcript = after.transcript();
        assert_lines_in_order(&transcript, "pre ", &numbered("pre ", 1..=40));
        assert_lines_in_order(&transcript, "post ", &numbered("post ", 1..=10));
        assert_no_ui_rows_in_history(&mut after);
        let visible = after.visible();
        let ui_top = usize::from(rows) - 5;
        assert!(
            (0..5).all(|y| visible[ui_top + y].starts_with(&format!("UI-A-row{y}"))),
            "{case}: the frame is repainted on the last 5 rows: {visible:?}"
        );
        assert_eq!(
            visible[ui_top - 1],
            "post 010",
            "{case}: the newest line sits right above the UI"
        );
    }
}

/// What tmux 3.6 shows after shrinking a pane to `rows`: it first deletes
/// the rows below the cursor (whatever they hold), then pushes rows off the
/// top into history; the cursor stays on its row's content.
fn tmux_shrink(emu: &mut Emulator, rows: u16) -> Emulator {
    let (cursor_row, _) = emu.cursor();
    let below = emu.rows - 1 - cursor_row;
    let deleted = (emu.rows - rows).min(below);
    let mut all = emu.transcript();
    let first_below = all.len() - usize::from(emu.rows) + usize::from(cursor_row) + 1;
    all.drain(first_below..first_below + usize::from(deleted));
    let mut resized = Emulator::new(emu.cols, rows);
    resized.feed(all.join("\r\n").as_bytes());
    let still_below = below - deleted;
    if still_below > 0 {
        resized.feed(format!("\x1b[{still_below}A").as_bytes());
    }
    resized
}

/// A visible input cursor above the UI's last row (agent_shell's input row
/// over its hint row) loses the rows below it when tmux shrinks the pane, so
/// the UI no longer ends on the last row. Before re-anchoring, the next
/// bottom-addressed erase started one row too high and took the newest log
/// line (the G42 height-only storm lost 16 of 100,000 this way).
#[test]
fn overlay_stream_survives_a_shrink_that_deletes_rows_below_the_cursor() {
    // Either the bytes arrive before the writer hears of the resize, or the
    // writer applies it first (its region cache is dropped, so the cursor's
    // row in the UI must survive `set_size`) and draws a new frame.
    for learns_size in [false, true] {
        let out = SharedOutput::default();
        let mut writer = TerminalWriter::new(
            out.clone(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        for i in 1..=40 {
            writer.write_log(&format!("pre {i:03}\n")).unwrap();
        }
        writer
            .present_ui(&ui_frame(80, 5, "A"), Some((2, 3)), true)
            .unwrap();
        writer.flush().unwrap();
        let mut before = Emulator::new(80, 24);
        before.feed(&out.bytes());
        assert_eq!(before.cursor(), (22, 2), "cursor on the UI's row 3 of 5");

        let mut after = tmux_shrink(&mut before, 21);
        assert_eq!(
            after.visible().last().map(String::as_str),
            Some("UI-A-row3"),
            "tmux deleted the UI row below the cursor"
        );
        let mark = out.len();
        if learns_size {
            writer.set_size(80, 21);
            writer
                .present_ui(&ui_frame(80, 5, "A"), Some((2, 3)), true)
                .unwrap();
        }
        for i in 1..=10 {
            writer.write_log(&format!("post {i:03}\n")).unwrap();
        }
        writer.flush().unwrap();
        after.feed(&out.bytes()[mark..]);

        let transcript = after.transcript();
        assert_lines_in_order(&transcript, "pre ", &numbered("pre ", 1..=40));
        assert_lines_in_order(&transcript, "post ", &numbered("post ", 1..=10));
        assert_no_ui_rows_in_history(&mut after);
        let visible = after.visible();
        assert!(
            (0..5).all(|y| visible[16 + y].starts_with(&format!("UI-A-row{y}"))),
            "learns_size={learns_size}: the frame is repainted on the last 5 rows: {visible:?}"
        );
        assert_eq!(
            visible[15], "post 010",
            "learns_size={learns_size}: the newest line sits right above the UI"
        );
    }
}
