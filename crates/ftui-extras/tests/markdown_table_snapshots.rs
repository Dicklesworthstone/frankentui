#![forbid(unsafe_code)]
//! Snapshot tests for markdown table rendering (bd-2k018.11).
//!
//! Run:
//!   cargo test -p ftui-extras --test markdown_table_snapshots --features markdown
//! Update snapshots:
//!   BLESS=1 cargo test -p ftui-extras --test markdown_table_snapshots --features markdown

#[cfg(feature = "markdown")]
use ftui_core::geometry::Rect;
#[cfg(feature = "markdown")]
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
#[cfg(feature = "markdown")]
use ftui_render::buffer::Buffer;
#[cfg(feature = "markdown")]
use ftui_render::cell::{PackedRgba, StyleFlags};
#[cfg(feature = "markdown")]
use ftui_render::frame::Frame;
#[cfg(feature = "markdown")]
use ftui_render::grapheme_pool::GraphemePool;
#[cfg(feature = "markdown")]
use ftui_text::Text;
#[cfg(feature = "markdown")]
use ftui_widgets::Widget;
#[cfg(feature = "markdown")]
use ftui_widgets::paragraph::Paragraph;
#[cfg(feature = "markdown")]
use std::fmt::Write as FmtWrite;
#[cfg(feature = "markdown")]
use std::path::Path;

#[cfg(feature = "markdown")]
fn render_markdown(markdown: &str, table_max_width: Option<u16>) -> Text {
    let renderer = MarkdownRenderer::new(MarkdownTheme::default());
    let renderer = match table_max_width {
        Some(width) => renderer.table_max_width(width),
        None => renderer,
    };
    renderer.render(markdown)
}

#[cfg(feature = "markdown")]
fn buffer_to_ansi(buf: &Buffer) -> String {
    let capacity = (buf.width() as usize + 32) * buf.height() as usize;
    let mut out = String::with_capacity(capacity);

    for y in 0..buf.height() {
        if y > 0 {
            out.push('\n');
        }

        let mut prev_fg = PackedRgba::WHITE;
        let mut prev_bg = PackedRgba::TRANSPARENT;
        let mut prev_flags = StyleFlags::empty();
        let mut style_active = false;

        for x in 0..buf.width() {
            let cell = buf.get(x, y).unwrap();
            if cell.is_continuation() {
                continue;
            }

            let fg = cell.fg;
            let bg = cell.bg;
            let flags = cell.attrs.flags();
            let style_changed = fg != prev_fg || bg != prev_bg || flags != prev_flags;

            if style_changed {
                let has_style =
                    fg != PackedRgba::WHITE || bg != PackedRgba::TRANSPARENT || !flags.is_empty();

                if has_style {
                    if style_active {
                        out.push_str("\x1b[0m");
                    }

                    let mut params: Vec<String> = Vec::new();
                    if !flags.is_empty() {
                        if flags.contains(StyleFlags::BOLD) {
                            params.push("1".to_string());
                        }
                        if flags.contains(StyleFlags::DIM) {
                            params.push("2".to_string());
                        }
                        if flags.contains(StyleFlags::ITALIC) {
                            params.push("3".to_string());
                        }
                        if flags.contains(StyleFlags::UNDERLINE) {
                            params.push("4".to_string());
                        }
                        if flags.contains(StyleFlags::BLINK) {
                            params.push("5".to_string());
                        }
                        if flags.contains(StyleFlags::REVERSE) {
                            params.push("7".to_string());
                        }
                        if flags.contains(StyleFlags::HIDDEN) {
                            params.push("8".to_string());
                        }
                        if flags.contains(StyleFlags::STRIKETHROUGH) {
                            params.push("9".to_string());
                        }
                    }

                    if fg.a() > 0 && fg != PackedRgba::WHITE {
                        params.push(format!("38;2;{};{};{}", fg.r(), fg.g(), fg.b()));
                    }
                    if bg.a() > 0 && bg != PackedRgba::TRANSPARENT {
                        params.push(format!("48;2;{};{};{}", bg.r(), bg.g(), bg.b()));
                    }

                    if params.is_empty() {
                        out.push_str("\x1b[0m");
                    } else {
                        out.push_str("\x1b[");
                        out.push_str(&params.join(";"));
                        out.push('m');
                    }
                    style_active = true;
                } else if style_active {
                    out.push_str("\x1b[0m");
                    style_active = false;
                }

                prev_fg = fg;
                prev_bg = bg;
                prev_flags = flags;
            }

            if let Some(ch) = cell.content.as_char() {
                out.push(ch);
            } else {
                out.push('?');
            }
        }

        if style_active {
            out.push_str("\x1b[0m");
        }
    }

    out
}

#[cfg(feature = "markdown")]
fn diff_text(expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();

    let max_lines = expected_lines.len().max(actual_lines.len());
    let mut out = String::new();
    let mut has_diff = false;

    for i in 0..max_lines {
        let exp = expected_lines.get(i).copied();
        let act = actual_lines.get(i).copied();

        match (exp, act) {
            (Some(e), Some(a)) if e == a => {
                writeln!(out, " {e}").unwrap();
            }
            (Some(e), Some(a)) => {
                writeln!(out, "-{e}").unwrap();
                writeln!(out, "+{a}").unwrap();
                has_diff = true;
            }
            (Some(e), None) => {
                writeln!(out, "-{e}").unwrap();
                has_diff = true;
            }
            (None, Some(a)) => {
                writeln!(out, "+{a}").unwrap();
                has_diff = true;
            }
            (None, None) => {}
        }
    }

    if has_diff { out } else { String::new() }
}

#[cfg(feature = "markdown")]
fn is_bless() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(feature = "markdown")]
fn assert_buffer_snapshot_ansi(name: &str, buf: &Buffer) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = base
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.ansi.snap"));
    let actual = buffer_to_ansi(buf);

    if is_bless() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create snapshot directory");
        }
        std::fs::write(&path, &actual).expect("failed to write snapshot");
        return;
    }

    match std::fs::read_to_string(&path) {
        Ok(expected) => {
            if expected != actual {
                let diff = diff_text(&expected, &actual);
                std::panic::panic_any(format!(
                    "=== ANSI snapshot mismatch: '{name}' ===\nFile: {}\nSet BLESS=1 to update.\n\nDiff (- expected, + actual):\n{diff}",
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::panic::panic_any(format!(
                "=== No ANSI snapshot found: '{name}' ===\nExpected at: {}\nRun with BLESS=1 to create it.\n\nActual output:\n{actual}",
                path.display()
            ));
        }
        Err(e) => {
            std::panic::panic_any(format!("Failed to read snapshot '{}': {e}", path.display()));
        }
    }
}

#[cfg(feature = "markdown")]
macro_rules! assert_snapshot_ansi {
    ($name:expr, $buf:expr) => {
        assert_buffer_snapshot_ansi($name, $buf)
    };
}

#[test]
#[cfg(feature = "markdown")]
fn snapshot_markdown_table_basic() {
    let md = "\
| Feature | Status | Notes |
|---|:---:|---:|
| Inline mode | OK | Scrollback preserved |
| Diff engine | OK | SIMD-friendly |
| Evidence logs | OK | JSONL output |
";
    let text = render_markdown(md, None);
    let width = text.width().max(1) as u16;
    let height = text.height().max(1) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    Paragraph::new(text).render(Rect::new(0, 0, width, height), &mut frame);
    assert_snapshot_ansi!("markdown_table_basic", &frame.buffer);
}

#[test]
#[cfg(feature = "markdown")]
fn snapshot_markdown_table_alignment() {
    let md = "\
| Left | Center | Right |
|:---|:---:|---:|
| L1 | C1 | R1 |
| L2 | C2 | R2 |
| L3 | C3 | R3 |
";
    let text = render_markdown(md, None);
    let width = text.width().max(1) as u16;
    let height = text.height().max(1) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    Paragraph::new(text).render(Rect::new(0, 0, width, height), &mut frame);
    assert_snapshot_ansi!("markdown_table_alignment", &frame.buffer);
}

#[test]
#[cfg(feature = "markdown")]
fn snapshot_markdown_table_max_width() {
    let md = "\
| Column | Description |
|---|---|
| Inline | This description is intentionally long to test truncation. |
| Diff | Another long description to exceed the max table width. |
";
    let text = render_markdown(md, Some(32));
    let width = 32u16;
    let height = text.height().max(1) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    Paragraph::new(text).render(Rect::new(0, 0, width, height), &mut frame);
    assert_snapshot_ansi!("markdown_table_max_width", &frame.buffer);
}
