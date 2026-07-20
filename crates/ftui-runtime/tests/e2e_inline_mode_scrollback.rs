#![forbid(unsafe_code)]

//! bd-1q5.17: E2E test: Inline mode in real terminal with scrollback.
//!
//! Covers:
//! 1. Write 100 lines of output to terminal
//! 2. Render inline widget
//! 3. Verify scrollback accessible above widget
//! 4. Remove widget, verify scrollback intact
//! 5. Assert inline.render span
//! 6. Assert scrollback_preserved=true
//!
//! Run:
//!   cargo test -p ftui-runtime --test e2e_inline_mode_scrollback

use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_render::buffer::Buffer;
use ftui_runtime::{ScreenMode, TerminalWriter, UiAnchor, inline_active_widgets};

// ============================================================================
// Helpers
// ============================================================================

fn basic_caps() -> TerminalCapabilities {
    TerminalCapabilities::basic()
}

fn full_caps() -> TerminalCapabilities {
    let mut caps = TerminalCapabilities::basic();
    caps.color_depth = ColorDepth::TrueColor;
    caps.sync_output = true;
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
/// DEC cursor save (ESC 7).
const CURSOR_SAVE: &[u8] = b"\x1b7";
/// DEC cursor restore (ESC 8).
const CURSOR_RESTORE: &[u8] = b"\x1b8";

// ============================================================================
// 1. Write 100 lines then render inline widget
// ============================================================================

#[test]
fn write_100_lines_then_render_inline_widget() {
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Write 100 lines to scrollback
        for i in 1..=100 {
            writer
                .write_log(&format!("scrollback line {i:03}\n"))
                .unwrap();
        }

        // Render the inline widget
        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();
    }

    let text = String::from_utf8_lossy(&output);

    // All 100 lines should be in the output
    for i in 1..=100 {
        assert!(
            text.contains(&format!("scrollback line {i:03}")),
            "scrollback line {i:03} must be present in output"
        );
    }

    // No alternate screen sequences
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
// 2. Scrollback preserved after inline render
// ============================================================================

#[test]
fn scrollback_accessible_above_widget() {
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Write scrollback lines
        for i in 1..=100 {
            writer.write_log(&format!("log line {i:03}\n")).unwrap();
        }

        // Render inline widget
        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();

        // Write more scrollback after render — must also survive
        writer.write_log("post-render log\n").unwrap();
    }

    let text = String::from_utf8_lossy(&output);

    // Scrollback lines before render
    assert!(text.contains("log line 001"), "first log must survive");
    assert!(text.contains("log line 050"), "middle log must survive");
    assert!(text.contains("log line 100"), "last log must survive");
    // Post-render log
    assert!(
        text.contains("post-render log"),
        "post-render log must survive"
    );

    // Cursor save/restore must bracket the UI render
    assert!(
        contains_bytes(&output, CURSOR_SAVE),
        "present_ui must save cursor to protect scrollback"
    );
    assert!(
        contains_bytes(&output, CURSOR_RESTORE),
        "present_ui must restore cursor to protect scrollback"
    );
}

// ============================================================================
// 3. Remove widget, verify scrollback intact
// ============================================================================

#[test]
fn scrollback_intact_after_widget_removal() {
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Write 100 lines
        for i in 1..=100 {
            writer
                .write_log(&format!("preserved line {i:03}\n"))
                .unwrap();
        }

        // Render widget
        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();

        // Writer drop == widget removal (RAII cleanup)
    }

    let text = String::from_utf8_lossy(&output);

    // All 100 lines must survive the full lifecycle (write → render → drop)
    for i in 1..=100 {
        assert!(
            text.contains(&format!("preserved line {i:03}")),
            "preserved line {i:03} must survive widget removal"
        );
    }

    // Must not use alternate screen at any point
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
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Interleave log writes and UI renders
        for batch in 0..10 {
            for i in 0..10 {
                let line_num = batch * 10 + i + 1;
                writer.write_log(&format!("line {line_num:03}\n")).unwrap();
            }
            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }
    }

    let text = String::from_utf8_lossy(&output);

    // All 100 lines across 10 batches must be present
    for i in 1..=100 {
        assert!(
            text.contains(&format!("line {i:03}")),
            "line {i:03} must survive interleaved render passes"
        );
    }
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
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Phase 1: Write 100 lines
        for i in 1..=100 {
            writer.write_log(&format!("scrollback {i:03}\n")).unwrap();
        }

        // Phase 2: First render
        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();

        // Phase 3: More log writes
        writer.write_log("mid-session log A\n").unwrap();
        writer.write_log("mid-session log B\n").unwrap();

        // Phase 4: Second render (diff path)
        let buffer2 = Buffer::new(80, 5);
        writer.present_ui(&buffer2, None, true).unwrap();

        // Phase 5: Writer drops → widget removed
    }

    let text = String::from_utf8_lossy(&output);

    // Scrollback lines survive the full lifecycle
    assert!(text.contains("scrollback 001"), "first line");
    assert!(text.contains("scrollback 050"), "middle line");
    assert!(text.contains("scrollback 100"), "last line");
    assert!(text.contains("mid-session log A"), "mid-session A");
    assert!(text.contains("mid-session log B"), "mid-session B");

    // No alternate screen
    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "must not enter alt screen"
    );

    // Cursor save/restore brackets the UI render
    assert!(
        contains_bytes(&output, CURSOR_SAVE),
        "must save cursor during inline render"
    );
    assert!(
        contains_bytes(&output, CURSOR_RESTORE),
        "must restore cursor during inline render"
    );
}

// ============================================================================
// 8. InlineAuto mode also preserves scrollback
// ============================================================================

#[test]
fn inline_auto_mode_preserves_scrollback() {
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 10,
            },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        for i in 1..=100 {
            writer.write_log(&format!("auto line {i:03}\n")).unwrap();
        }

        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();
    }

    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("auto line 001"), "first line in InlineAuto");
    assert!(text.contains("auto line 100"), "last line in InlineAuto");

    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "InlineAuto must never emit alternate screen enter"
    );
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
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            full_caps(),
        );
        writer.set_size(80, 24);

        for i in 1..=100 {
            writer.write_log(&format!("sync line {i:03}\n")).unwrap();
        }

        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();
    }

    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("sync line 001"), "first line with sync caps");
    assert!(text.contains("sync line 100"), "last line with sync caps");

    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "inline mode must never emit alternate screen even with full caps"
    );

    // Cursor save/restore must still bracket the render
    assert!(
        contains_bytes(&output, CURSOR_SAVE),
        "must save cursor with full caps"
    );
    assert!(
        contains_bytes(&output, CURSOR_RESTORE),
        "must restore cursor with full caps"
    );
}

// ============================================================================
// 11. Resize between renders preserves scrollback
// ============================================================================

#[test]
fn resize_between_renders_preserves_scrollback() {
    let mut output = Vec::new();
    {
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Write initial scrollback
        for i in 1..=50 {
            writer
                .write_log(&format!("before resize {i:03}\n"))
                .unwrap();
        }

        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, true).unwrap();

        // Resize
        writer.set_size(120, 40);

        // Write more scrollback after resize
        for i in 51..=100 {
            writer.write_log(&format!("after resize {i:03}\n")).unwrap();
        }

        let buffer2 = Buffer::new(120, 5);
        writer.present_ui(&buffer2, None, true).unwrap();
    }

    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("before resize 001"),
        "pre-resize scrollback must survive"
    );
    assert!(
        text.contains("before resize 050"),
        "last pre-resize line must survive"
    );
    assert!(
        text.contains("after resize 051"),
        "first post-resize line must survive"
    );
    assert!(
        text.contains("after resize 100"),
        "last post-resize line must survive"
    );

    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "resize must not trigger alternate screen"
    );
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

    let buffer = Buffer::new(80, 5);
    writer.present_ui(&buffer, None, true).unwrap();

    // into_inner performs cleanup and returns the underlying writer
    let output = writer.into_inner().expect("into_inner should succeed");
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("inner line 001"), "first line via into_inner");
    assert!(text.contains("inner line 100"), "last line via into_inner");

    assert!(
        !contains_bytes(&output, ALTSCREEN_ENTER),
        "into_inner must not emit alternate screen"
    );
}
