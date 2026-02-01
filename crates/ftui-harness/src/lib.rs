#![forbid(unsafe_code)]

//! Snapshot/golden testing and time-travel debugging for FrankenTUI.
//!
//! - **Snapshot testing**: Captures `Buffer` output as text, compares against stored `.snap` files.
//! - **Time-travel debugging**: Records compressed frame snapshots for rewind inspection.
//!
//! Captures `Buffer` output as plain text or ANSI-styled text, compares
//! against stored snapshots, and shows diffs on mismatch.
//!
//! # Quick Start
//!
//! ```ignore
//! use ftui_harness::{assert_snapshot, MatchMode};
//!
//! #[test]
//! fn my_widget_renders_correctly() {
//!     let mut buf = Buffer::new(10, 3);
//!     // ... render widget into buf ...
//!     assert_snapshot!("my_widget_basic", &buf);
//! }
//! ```
//!
//! # Updating Snapshots
//!
//! Run tests with `BLESS=1` to create or update snapshot files:
//!
//! ```sh
//! BLESS=1 cargo test
//! ```
//!
//! Snapshot files are stored under `tests/snapshots/` relative to the
//! crate's `CARGO_MANIFEST_DIR`.

pub mod asciicast;
pub mod time_travel;
pub mod time_travel_inspector;

#[cfg(feature = "pty-capture")]
pub mod pty_capture;

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use ftui_render::buffer::Buffer;
use ftui_render::cell::{PackedRgba, StyleFlags};

// Re-export types useful for harness users.
pub use ftui_core::geometry::Rect;
pub use ftui_render::buffer;
pub use ftui_render::cell;
pub use time_travel_inspector::TimeTravelInspector;

// ============================================================================
// Buffer → Text Conversion
// ============================================================================

/// Convert a `Buffer` to a plain text string.
///
/// Each row becomes one line. Empty cells become spaces. Continuation cells
/// (trailing cells of wide characters) are skipped so wide characters occupy
/// their natural display width in the output string.
///
/// Grapheme-pool references (multi-codepoint clusters) are rendered as `?`
/// since the pool is not available here.
pub fn buffer_to_text(buf: &Buffer) -> String {
    let capacity = (buf.width() as usize + 1) * buf.height() as usize;
    let mut out = String::with_capacity(capacity);

    for y in 0..buf.height() {
        if y > 0 {
            out.push('\n');
        }
        for x in 0..buf.width() {
            let cell = buf.get(x, y).unwrap();
            if cell.is_continuation() {
                continue;
            }
            if cell.is_empty() {
                out.push(' ');
            } else if let Some(c) = cell.content.as_char() {
                out.push(c);
            } else {
                // Grapheme ID — pool not available, use placeholder
                out.push('?');
            }
        }
    }
    out
}

/// Convert a `Buffer` to text with inline ANSI escape codes.
///
/// Emits SGR sequences when foreground, background, or style flags change
/// between adjacent cells. Resets styling at the end of each row.
pub fn buffer_to_ansi(buf: &Buffer) -> String {
    let capacity = (buf.width() as usize + 32) * buf.height() as usize;
    let mut out = String::with_capacity(capacity);

    for y in 0..buf.height() {
        if y > 0 {
            out.push('\n');
        }

        let mut prev_fg = PackedRgba::WHITE; // Cell default fg
        let mut prev_bg = PackedRgba::TRANSPARENT; // Cell default bg
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
                    // Reset and re-emit
                    if style_active {
                        out.push_str("\x1b[0m");
                    }

                    let mut params: Vec<String> = Vec::new();
                    if !flags.is_empty() {
                        if flags.contains(StyleFlags::BOLD) {
                            params.push("1".into());
                        }
                        if flags.contains(StyleFlags::DIM) {
                            params.push("2".into());
                        }
                        if flags.contains(StyleFlags::ITALIC) {
                            params.push("3".into());
                        }
                        if flags.contains(StyleFlags::UNDERLINE) {
                            params.push("4".into());
                        }
                        if flags.contains(StyleFlags::BLINK) {
                            params.push("5".into());
                        }
                        if flags.contains(StyleFlags::REVERSE) {
                            params.push("7".into());
                        }
                        if flags.contains(StyleFlags::HIDDEN) {
                            params.push("8".into());
                        }
                        if flags.contains(StyleFlags::STRIKETHROUGH) {
                            params.push("9".into());
                        }
                    }
                    if fg.a() > 0 && fg != PackedRgba::WHITE {
                        params.push(format!("38;2;{};{};{}", fg.r(), fg.g(), fg.b()));
                    }
                    if bg.a() > 0 && bg != PackedRgba::TRANSPARENT {
                        params.push(format!("48;2;{};{};{}", bg.r(), bg.g(), bg.b()));
                    }

                    if !params.is_empty() {
                        write!(out, "\x1b[{}m", params.join(";")).unwrap();
                        style_active = true;
                    }
                } else if style_active {
                    out.push_str("\x1b[0m");
                    style_active = false;
                }

                prev_fg = fg;
                prev_bg = bg;
                prev_flags = flags;
            }

            if cell.is_empty() {
                out.push(' ');
            } else if let Some(c) = cell.content.as_char() {
                out.push(c);
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

// ============================================================================
// Match Modes & Normalization
// ============================================================================

/// Comparison mode for snapshot testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// Byte-exact string comparison.
    Exact,
    /// Trim trailing whitespace on each line before comparing.
    TrimTrailing,
    /// Collapse all whitespace runs to single spaces and trim each line.
    Fuzzy,
}

/// Normalize text according to the requested match mode.
fn normalize(text: &str, mode: MatchMode) -> String {
    match mode {
        MatchMode::Exact => text.to_string(),
        MatchMode::TrimTrailing => text
            .lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n"),
        MatchMode::Fuzzy => text
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

// ============================================================================
// Diff
// ============================================================================

/// Compute a simple line-by-line diff between two text strings.
///
/// Returns a human-readable string where:
/// - Lines prefixed with ` ` are identical in both.
/// - Lines prefixed with `-` appear only in `expected`.
/// - Lines prefixed with `+` appear only in `actual`.
///
/// Returns an empty string when the inputs are identical.
pub fn diff_text(expected: &str, actual: &str) -> String {
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

// ============================================================================
// Snapshot Assertion
// ============================================================================

/// Resolve the snapshot file path.
fn snapshot_path(base_dir: &Path, name: &str) -> PathBuf {
    base_dir
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.snap"))
}

/// Check if the `BLESS` environment variable is set.
fn is_bless() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Assert that a buffer's text representation matches a stored snapshot.
///
/// # Arguments
///
/// * `name`     – Snapshot identifier (used as the `.snap` filename).
/// * `buf`      – The buffer to compare.
/// * `base_dir` – Root directory for snapshot storage (use `env!("CARGO_MANIFEST_DIR")`).
/// * `mode`     – How to compare the text (exact, trim trailing, or fuzzy).
///
/// # Panics
///
/// * If the snapshot file does not exist and `BLESS=1` is **not** set.
/// * If the buffer output does not match the stored snapshot.
///
/// # Updating Snapshots
///
/// Set `BLESS=1` to write the current buffer output as the new snapshot:
///
/// ```sh
/// BLESS=1 cargo test
/// ```
pub fn assert_buffer_snapshot(name: &str, buf: &Buffer, base_dir: &str, mode: MatchMode) {
    let base = Path::new(base_dir);
    let path = snapshot_path(base, name);
    let actual = buffer_to_text(buf);

    if is_bless() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create snapshot directory");
        }
        std::fs::write(&path, &actual).expect("failed to write snapshot");
        return;
    }

    match std::fs::read_to_string(&path) {
        Ok(expected) => {
            let norm_expected = normalize(&expected, mode);
            let norm_actual = normalize(&actual, mode);

            if norm_expected != norm_actual {
                let diff = diff_text(&norm_expected, &norm_actual);
                panic!(
                    "\n\
                     === Snapshot mismatch: '{name}' ===\n\
                     File: {}\n\
                     Mode: {mode:?}\n\
                     Set BLESS=1 to update.\n\n\
                     Diff (- expected, + actual):\n{diff}",
                    path.display()
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            panic!(
                "\n\
                 === No snapshot found: '{name}' ===\n\
                 Expected at: {}\n\
                 Run with BLESS=1 to create it.\n\n\
                 Actual output ({w}x{h}):\n{actual}",
                path.display(),
                w = buf.width(),
                h = buf.height(),
            );
        }
        Err(e) => {
            panic!("Failed to read snapshot '{}': {e}", path.display());
        }
    }
}

/// Assert that a buffer's ANSI-styled representation matches a stored snapshot.
///
/// Behaves like [`assert_buffer_snapshot`] but captures ANSI escape codes.
/// Snapshot files have the `.ansi.snap` suffix.
pub fn assert_buffer_snapshot_ansi(name: &str, buf: &Buffer, base_dir: &str) {
    let base = Path::new(base_dir);
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
                panic!(
                    "\n\
                     === ANSI snapshot mismatch: '{name}' ===\n\
                     File: {}\n\
                     Set BLESS=1 to update.\n\n\
                     Diff (- expected, + actual):\n{diff}",
                    path.display()
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            panic!(
                "\n\
                 === No ANSI snapshot found: '{name}' ===\n\
                 Expected at: {}\n\
                 Run with BLESS=1 to create it.\n\n\
                 Actual output:\n{actual}",
                path.display(),
            );
        }
        Err(e) => {
            panic!("Failed to read snapshot '{}': {e}", path.display());
        }
    }
}

// ============================================================================
// Convenience Macros
// ============================================================================

/// Assert that a buffer matches a stored snapshot (plain text).
///
/// Uses `CARGO_MANIFEST_DIR` to locate the snapshot directory automatically.
///
/// # Examples
///
/// ```ignore
/// // Default mode: TrimTrailing
/// assert_snapshot!("widget_basic", &buf);
///
/// // Explicit mode
/// assert_snapshot!("widget_exact", &buf, MatchMode::Exact);
/// ```
#[macro_export]
macro_rules! assert_snapshot {
    ($name:expr, $buf:expr) => {
        $crate::assert_buffer_snapshot(
            $name,
            $buf,
            env!("CARGO_MANIFEST_DIR"),
            $crate::MatchMode::TrimTrailing,
        )
    };
    ($name:expr, $buf:expr, $mode:expr) => {
        $crate::assert_buffer_snapshot($name, $buf, env!("CARGO_MANIFEST_DIR"), $mode)
    };
}

/// Assert that a buffer matches a stored ANSI snapshot (with style info).
///
/// Uses `CARGO_MANIFEST_DIR` to locate the snapshot directory automatically.
#[macro_export]
macro_rules! assert_snapshot_ansi {
    ($name:expr, $buf:expr) => {
        $crate::assert_buffer_snapshot_ansi($name, $buf, env!("CARGO_MANIFEST_DIR"))
    };
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;

    #[test]
    fn buffer_to_text_empty() {
        let buf = Buffer::new(5, 2);
        let text = buffer_to_text(&buf);
        assert_eq!(text, "     \n     ");
    }

    #[test]
    fn buffer_to_text_simple() {
        let mut buf = Buffer::new(5, 1);
        buf.set(0, 0, Cell::from_char('H'));
        buf.set(1, 0, Cell::from_char('i'));
        let text = buffer_to_text(&buf);
        assert_eq!(text, "Hi   ");
    }

    #[test]
    fn buffer_to_text_multiline() {
        let mut buf = Buffer::new(3, 2);
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(1, 0, Cell::from_char('B'));
        buf.set(0, 1, Cell::from_char('C'));
        let text = buffer_to_text(&buf);
        assert_eq!(text, "AB \nC  ");
    }

    #[test]
    fn buffer_to_text_wide_char() {
        let mut buf = Buffer::new(4, 1);
        // '中' is width 2 — head at x=0, continuation at x=1
        buf.set(0, 0, Cell::from_char('中'));
        buf.set(2, 0, Cell::from_char('!'));
        let text = buffer_to_text(&buf);
        // '中' occupies 1 char in text, continuation skipped, '!' at col 2, space at col 3
        assert_eq!(text, "中! ");
    }

    #[test]
    fn buffer_to_ansi_no_style() {
        let mut buf = Buffer::new(3, 1);
        buf.set(0, 0, Cell::from_char('X'));
        let ansi = buffer_to_ansi(&buf);
        // No style changes from default → no escape codes
        assert_eq!(ansi, "X  ");
    }

    #[test]
    fn buffer_to_ansi_with_style() {
        let mut buf = Buffer::new(3, 1);
        let styled = Cell::from_char('R').with_fg(PackedRgba::rgb(255, 0, 0));
        buf.set(0, 0, styled);
        let ansi = buffer_to_ansi(&buf);
        // Should contain SGR for red foreground
        assert!(ansi.contains("\x1b[38;2;255;0;0m"));
        assert!(ansi.contains('R'));
        // Should end with reset
        assert!(ansi.contains("\x1b[0m"));
    }

    #[test]
    fn diff_text_identical() {
        let diff = diff_text("hello\nworld", "hello\nworld");
        assert!(diff.is_empty());
    }

    #[test]
    fn diff_text_single_line_change() {
        let diff = diff_text("hello\nworld", "hello\nearth");
        assert!(diff.contains("-world"));
        assert!(diff.contains("+earth"));
        assert!(diff.contains(" hello"));
    }

    #[test]
    fn diff_text_added_lines() {
        let diff = diff_text("A", "A\nB");
        assert!(diff.contains("+B"));
    }

    #[test]
    fn diff_text_removed_lines() {
        let diff = diff_text("A\nB", "A");
        assert!(diff.contains("-B"));
    }

    #[test]
    fn normalize_exact() {
        let text = "  hello  \n  world  ";
        assert_eq!(normalize(text, MatchMode::Exact), text);
    }

    #[test]
    fn normalize_trim_trailing() {
        let text = "hello  \n  world  ";
        assert_eq!(normalize(text, MatchMode::TrimTrailing), "hello\n  world");
    }

    #[test]
    fn normalize_fuzzy() {
        let text = "  hello   world  \n  foo   bar  ";
        assert_eq!(normalize(text, MatchMode::Fuzzy), "hello world\nfoo bar");
    }

    #[test]
    fn snapshot_path_construction() {
        let p = snapshot_path(Path::new("/crates/my-crate"), "widget_test");
        assert_eq!(
            p,
            PathBuf::from("/crates/my-crate/tests/snapshots/widget_test.snap")
        );
    }

    #[test]
    fn bless_creates_snapshot() {
        let dir = std::env::temp_dir().join("ftui_harness_test_bless");
        let _ = std::fs::remove_dir_all(&dir);

        let mut buf = Buffer::new(3, 1);
        buf.set(0, 0, Cell::from_char('X'));

        // Simulate BLESS=1 by writing directly
        let path = snapshot_path(&dir, "bless_test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = buffer_to_text(&buf);
        std::fs::write(&path, &text).unwrap();

        // Verify file was created with correct content
        let stored = std::fs::read_to_string(&path).unwrap();
        assert_eq!(stored, "X  ");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_match_succeeds() {
        let dir = std::env::temp_dir().join("ftui_harness_test_match");
        let _ = std::fs::remove_dir_all(&dir);

        let mut buf = Buffer::new(5, 1);
        buf.set(0, 0, Cell::from_char('O'));
        buf.set(1, 0, Cell::from_char('K'));

        // Write snapshot
        let path = snapshot_path(&dir, "match_test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "OK   ").unwrap();

        // Assert should pass
        assert_buffer_snapshot("match_test", &buf, dir.to_str().unwrap(), MatchMode::Exact);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_trim_trailing_mode() {
        let dir = std::env::temp_dir().join("ftui_harness_test_trim");
        let _ = std::fs::remove_dir_all(&dir);

        let mut buf = Buffer::new(5, 1);
        buf.set(0, 0, Cell::from_char('A'));

        // Stored snapshot has no trailing spaces
        let path = snapshot_path(&dir, "trim_test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "A").unwrap();

        // Should match because TrimTrailing strips trailing spaces
        assert_buffer_snapshot(
            "trim_test",
            &buf,
            dir.to_str().unwrap(),
            MatchMode::TrimTrailing,
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[should_panic(expected = "Snapshot mismatch")]
    fn snapshot_mismatch_panics() {
        let dir = std::env::temp_dir().join("ftui_harness_test_mismatch");
        let _ = std::fs::remove_dir_all(&dir);

        let mut buf = Buffer::new(3, 1);
        buf.set(0, 0, Cell::from_char('X'));

        // Write mismatching snapshot
        let path = snapshot_path(&dir, "mismatch_test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "Y  ").unwrap();

        assert_buffer_snapshot(
            "mismatch_test",
            &buf,
            dir.to_str().unwrap(),
            MatchMode::Exact,
        );
    }

    #[test]
    #[should_panic(expected = "No snapshot found")]
    fn missing_snapshot_panics() {
        let dir = std::env::temp_dir().join("ftui_harness_test_missing");
        let _ = std::fs::remove_dir_all(&dir);

        let buf = Buffer::new(3, 1);
        assert_buffer_snapshot("nonexistent", &buf, dir.to_str().unwrap(), MatchMode::Exact);
    }
}
