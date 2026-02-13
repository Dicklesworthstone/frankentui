//! Export adapters for converting terminal grid state to external formats.
//!
//! This module defines the API contract, option schemas, and shared
//! infrastructure for three export paths:
//!
//! - **ANSI**: Re-emit grid content as VT/ANSI escape sequences.
//! - **HTML**: Render grid as styled HTML with span elements.
//! - **Plain text**: Extract text content without formatting.
//!
//! # Architecture
//!
//! An [`ExportContext`] bundles borrowed references to the terminal data
//! sources (grid, scrollback, hyperlink registry). Each export function
//! takes the context plus format-specific options and returns a `String`.
//!
//! The [`ExportRange`] type controls which lines are included in the
//! output, supporting viewport-only, scrollback-only, full history, or
//! an arbitrary line range.

use std::fmt::Write;
use std::ops::Range;

use crate::cell::{Cell, CellFlags, Color, HyperlinkRegistry, SgrFlags};
use crate::grid::Grid;
use crate::scrollback::Scrollback;

// ── Shared types ─────────────────────────────────────────────────────

/// Which lines to include in the export.
///
/// Line indices in the combined buffer: `0..scrollback.len()` for scrollback
/// (oldest first), followed by grid viewport rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ExportRange {
    /// Only the visible viewport (grid rows).
    #[default]
    Viewport,
    /// Only scrollback lines.
    ScrollbackOnly,
    /// Both scrollback and viewport (full history).
    Full,
    /// A specific line range in the combined buffer.
    Lines(Range<u32>),
}

/// Line ending style for exported text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// Unix-style `\n`.
    #[default]
    Lf,
    /// Windows-style `\r\n`.
    CrLf,
}

impl LineEnding {
    /// The string representation of this line ending.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// Color depth for ANSI export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    /// No color output (SGR attributes only: bold, italic, etc.).
    NoColor,
    /// 16-color palette (SGR 30–37, 40–47, 90–97, 100–107).
    Named16,
    /// 256-color palette (SGR 38;5;N / 48;5;N).
    Indexed256,
    /// 24-bit true color (SGR 38;2;R;G;B / 48;2;R;G;B).
    #[default]
    TrueColor,
}

// ── ANSI export options ──────────────────────────────────────────────

/// Options for ANSI escape sequence export.
///
/// Controls color depth, line handling, and reset behavior for
/// re-emitting grid content as VT/ANSI sequences.
#[derive(Debug, Clone)]
pub struct AnsiExportOptions {
    /// Which lines to include.
    pub range: ExportRange,
    /// Maximum color depth to emit.
    pub color_depth: ColorDepth,
    /// Line ending style.
    pub line_ending: LineEnding,
    /// Trim trailing whitespace from each line.
    pub trim_trailing: bool,
    /// Emit SGR 0 (reset) at the end of output.
    pub reset_at_end: bool,
    /// Join soft-wrapped scrollback lines without inserting a newline.
    pub join_soft_wraps: bool,
}

impl Default for AnsiExportOptions {
    fn default() -> Self {
        Self {
            range: ExportRange::Viewport,
            color_depth: ColorDepth::TrueColor,
            line_ending: LineEnding::Lf,
            trim_trailing: true,
            reset_at_end: true,
            join_soft_wraps: true,
        }
    }
}

// ── HTML export options ──────────────────────────────────────────────

/// Options for HTML export.
///
/// Controls styling mode, hyperlink rendering, and HTML structure
/// for converting grid content to styled `<pre>` + `<span>` HTML.
#[derive(Debug, Clone)]
pub struct HtmlExportOptions {
    /// Which lines to include.
    pub range: ExportRange,
    /// Use inline styles (`true`) or CSS classes (`false`).
    pub inline_styles: bool,
    /// CSS class prefix for generated elements.
    pub class_prefix: String,
    /// Font family for the wrapper `<pre>` element.
    pub font_family: String,
    /// Font size (CSS value) for the wrapper.
    pub font_size: String,
    /// Render hyperlinks as `<a>` tags.
    pub render_hyperlinks: bool,
    /// Line ending style within the HTML output.
    pub line_ending: LineEnding,
    /// Trim trailing whitespace from each line.
    pub trim_trailing: bool,
}

impl Default for HtmlExportOptions {
    fn default() -> Self {
        Self {
            range: ExportRange::Viewport,
            inline_styles: true,
            class_prefix: "ft".into(),
            font_family: "monospace".into(),
            font_size: "14px".into(),
            render_hyperlinks: true,
            line_ending: LineEnding::Lf,
            trim_trailing: true,
        }
    }
}

// ── Text export options ──────────────────────────────────────────────

/// Options for plain text export.
///
/// Controls whitespace handling, soft-wrap joining, and combining
/// mark inclusion for extracting text from grid content.
#[derive(Debug, Clone)]
pub struct TextExportOptions {
    /// Which lines to include.
    pub range: ExportRange,
    /// Line ending style.
    pub line_ending: LineEnding,
    /// Trim trailing whitespace from each line.
    pub trim_trailing: bool,
    /// Join soft-wrapped scrollback lines without inserting a newline.
    pub join_soft_wraps: bool,
    /// Include combining marks in output.
    pub include_combining: bool,
}

impl Default for TextExportOptions {
    fn default() -> Self {
        Self {
            range: ExportRange::Viewport,
            line_ending: LineEnding::Lf,
            trim_trailing: true,
            join_soft_wraps: true,
            include_combining: true,
        }
    }
}

// ── Export context ────────────────────────────────────────────────────

/// Bundles borrowed references to terminal data sources for export.
///
/// All fields are immutable borrows — the exporter never mutates
/// terminal state.
pub struct ExportContext<'a> {
    /// The visible viewport grid.
    pub grid: &'a Grid,
    /// The scrollback buffer.
    pub scrollback: &'a Scrollback,
    /// The hyperlink URI registry (for OSC 8 links).
    pub hyperlinks: &'a HyperlinkRegistry,
}

impl<'a> ExportContext<'a> {
    /// Create a new export context.
    #[must_use]
    pub fn new(
        grid: &'a Grid,
        scrollback: &'a Scrollback,
        hyperlinks: &'a HyperlinkRegistry,
    ) -> Self {
        Self {
            grid,
            scrollback,
            hyperlinks,
        }
    }
}

// ── Shared row resolution ────────────────────────────────────────────

/// A single resolved row for export: cell data plus soft-wrap flag.
#[derive(Debug, Clone)]
pub struct ExportRow<'a> {
    /// The cells of this row.
    pub cells: &'a [Cell],
    /// Whether the *next* line continues this one (soft-wrap).
    /// For viewport rows, this is always `false`.
    pub is_soft_wrapped: bool,
}

/// Resolve an [`ExportRange`] into a sequence of rows from the combined
/// buffer (scrollback + viewport).
///
/// The `is_soft_wrapped` flag on each row indicates whether the
/// *following* line has `wrapped=true`, meaning this row should be
/// joined with the next without a newline separator.
#[must_use]
pub fn resolve_rows<'a>(
    grid: &'a Grid,
    scrollback: &'a Scrollback,
    range: &ExportRange,
) -> Vec<ExportRow<'a>> {
    match range {
        ExportRange::Viewport => (0..grid.rows())
            .filter_map(|r| {
                grid.row_cells(r).map(|cells| ExportRow {
                    cells,
                    is_soft_wrapped: false,
                })
            })
            .collect(),
        ExportRange::ScrollbackOnly => resolve_scrollback_rows(scrollback),
        ExportRange::Full => {
            let mut rows = resolve_scrollback_rows(scrollback);
            // The last scrollback row might be soft-wrapped into the
            // first viewport row — check the first viewport row's
            // implied "unwrapped" status. We leave scrollback's
            // is_soft_wrapped as computed (based on next scrollback line).
            for r in 0..grid.rows() {
                if let Some(cells) = grid.row_cells(r) {
                    rows.push(ExportRow {
                        cells,
                        is_soft_wrapped: false,
                    });
                }
            }
            rows
        }
        ExportRange::Lines(line_range) => {
            let sb_len = scrollback.len() as u32;
            let mut rows = Vec::new();
            for line_idx in line_range.start..line_range.end {
                if line_idx < sb_len {
                    if let Some(sb_line) = scrollback.get(line_idx as usize) {
                        // Check if the next line is a soft-wrap continuation.
                        let next_wrapped = scrollback
                            .get(line_idx as usize + 1)
                            .is_some_and(|next| next.wrapped);
                        rows.push(ExportRow {
                            cells: &sb_line.cells,
                            is_soft_wrapped: next_wrapped,
                        });
                    }
                } else {
                    let grid_row = (line_idx - sb_len) as u16;
                    if let Some(cells) = grid.row_cells(grid_row) {
                        rows.push(ExportRow {
                            cells,
                            is_soft_wrapped: false,
                        });
                    }
                }
            }
            rows
        }
    }
}

/// Helper: resolve all scrollback lines with correct soft-wrap flags.
fn resolve_scrollback_rows(scrollback: &Scrollback) -> Vec<ExportRow<'_>> {
    let len = scrollback.len();
    let mut rows = Vec::with_capacity(len);
    for i in 0..len {
        let line = scrollback.get(i).unwrap();
        // A line is soft-wrapped if the *next* line has wrapped=true.
        let next_wrapped = scrollback.get(i + 1).is_some_and(|next| next.wrapped);
        rows.push(ExportRow {
            cells: &line.cells,
            is_soft_wrapped: next_wrapped,
        });
    }
    rows
}

// ── Text export (reference implementation) ───────────────────────────

/// Extract text from a single row of cells.
///
/// Skips wide-char continuation cells (so wide chars appear once).
/// Optionally includes combining marks. Optionally trims trailing spaces.
fn row_cells_to_text(cells: &[Cell], include_combining: bool, trim_trailing: bool) -> String {
    let mut buf = String::with_capacity(cells.len());
    for cell in cells {
        if cell.flags.contains(CellFlags::WIDE_CONTINUATION) {
            continue;
        }
        buf.push(cell.content());
        if include_combining {
            for &mark in cell.combining_marks() {
                buf.push(mark);
            }
        }
    }
    if trim_trailing {
        let trimmed_len = buf.trim_end_matches(' ').len();
        buf.truncate(trimmed_len);
    }
    buf
}

/// Export terminal content as plain text.
///
/// Wide-char continuation cells are skipped (each wide char appears once).
/// Trailing spaces are trimmed per line if configured. Soft-wrapped
/// scrollback lines are joined without a newline if configured.
#[must_use]
pub fn export_text(ctx: &ExportContext<'_>, opts: &TextExportOptions) -> String {
    let rows = resolve_rows(ctx.grid, ctx.scrollback, &opts.range);
    if rows.is_empty() {
        return String::new();
    }

    let line_end = opts.line_ending.as_str();
    let mut out = String::new();

    for (i, row) in rows.iter().enumerate() {
        let text = row_cells_to_text(row.cells, opts.include_combining, opts.trim_trailing);
        out.push_str(&text);

        // Insert line ending unless:
        // - This is the last row, OR
        // - This row is soft-wrapped and join_soft_wraps is enabled.
        if i + 1 < rows.len() {
            let skip_newline = opts.join_soft_wraps && row.is_soft_wrapped;
            if !skip_newline {
                out.push_str(line_end);
            }
        }
    }

    out
}

// ── ANSI export (signature — implementation in bd-2vr05.5.2) ─────────

/// Export terminal content as ANSI escape sequences.
///
/// Produces a byte-accurate VT/ANSI representation of the grid content
/// with style transitions (SGR), color sequences, and optional resets.
///
/// # Note
///
/// Full implementation is tracked in bd-2vr05.5.2. This function
/// currently delegates to a minimal stub that emits plain text with
/// SGR reset markers.
#[must_use]
pub fn export_ansi(ctx: &ExportContext<'_>, opts: &AnsiExportOptions) -> String {
    // Minimal stub: emit text with SGR reset at end.
    // Full ANSI serializer with style transitions in bd-2vr05.5.2.
    let text_opts = TextExportOptions {
        range: opts.range.clone(),
        line_ending: opts.line_ending,
        trim_trailing: opts.trim_trailing,
        join_soft_wraps: opts.join_soft_wraps,
        include_combining: true,
    };
    let mut out = export_text(ctx, &text_opts);
    if opts.reset_at_end {
        out.push_str("\x1b[0m");
    }
    out
}

/// Format a [`Color`] as an ANSI SGR parameter string for the given layer.
///
/// `layer` is `38` for foreground, `48` for background, `58` for underline.
/// Returns `None` for [`Color::Default`] (caller should emit SGR 39/49/59).
///
/// This helper is public so downstream ANSI export implementations
/// (bd-2vr05.5.2) can reuse it.
#[must_use]
pub fn color_to_sgr(color: Color, layer: u8, depth: ColorDepth) -> Option<String> {
    match depth {
        ColorDepth::NoColor => None,
        ColorDepth::Named16 => match color {
            Color::Default => None,
            Color::Named(n) if n < 8 => {
                let base = if layer == 38 { 30 } else { 40 };
                Some(format!("{}", base + n))
            }
            Color::Named(n) if n < 16 => {
                let base = if layer == 38 { 90 } else { 100 };
                Some(format!("{}", base + (n - 8)))
            }
            // Downgrade indexed/RGB to default in 16-color mode.
            _ => None,
        },
        ColorDepth::Indexed256 => match color {
            Color::Default => None,
            Color::Named(n) => Some(format!("{layer};5;{n}")),
            Color::Indexed(n) => Some(format!("{layer};5;{n}")),
            Color::Rgb(r, g, b) => {
                // Approximate RGB to 256-color cube.
                let idx = rgb_to_256(r, g, b);
                Some(format!("{layer};5;{idx}"))
            }
        },
        ColorDepth::TrueColor => match color {
            Color::Default => None,
            Color::Named(n) => Some(format!("{layer};5;{n}")),
            Color::Indexed(n) => Some(format!("{layer};5;{n}")),
            Color::Rgb(r, g, b) => Some(format!("{layer};2;{r};{g};{b}")),
        },
    }
}

/// Approximate an RGB color to the nearest 256-color palette index.
///
/// Uses the standard 6x6x6 color cube (indices 16–231) and the
/// 24-step grayscale ramp (indices 232–255).
#[must_use]
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Check if it's close to a grayscale value.
    if r == g && g == b {
        if r < 8 {
            return 16; // Closest to black in the cube.
        }
        if r > 248 {
            return 231; // Closest to white in the cube.
        }
        // Map to grayscale ramp: 232 + round((r - 8) / 247 * 23).
        return 232 + (((r as u16 - 8) * 23 + 123) / 247) as u8;
    }
    // Map to 6x6x6 color cube.
    let ri = ((r as u16 * 5 + 127) / 255) as u8;
    let gi = ((g as u16 * 5 + 127) / 255) as u8;
    let bi = ((b as u16 * 5 + 127) / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}

/// Format [`SgrFlags`] as a sequence of SGR parameter numbers.
///
/// Returns a `Vec` of SGR parameter values that should be included
/// in a `CSI ... m` sequence. Empty if no flags are set.
#[must_use]
pub fn sgr_flags_to_params(flags: SgrFlags) -> Vec<u8> {
    let mut params = Vec::new();
    if flags.contains(SgrFlags::BOLD) {
        params.push(1);
    }
    if flags.contains(SgrFlags::DIM) {
        params.push(2);
    }
    if flags.contains(SgrFlags::ITALIC) {
        params.push(3);
    }
    if flags.contains(SgrFlags::UNDERLINE) {
        params.push(4);
    }
    if flags.contains(SgrFlags::BLINK) {
        params.push(5);
    }
    if flags.contains(SgrFlags::INVERSE) {
        params.push(7);
    }
    if flags.contains(SgrFlags::HIDDEN) {
        params.push(8);
    }
    if flags.contains(SgrFlags::STRIKETHROUGH) {
        params.push(9);
    }
    if flags.contains(SgrFlags::DOUBLE_UNDERLINE) {
        params.push(21);
    }
    if flags.contains(SgrFlags::OVERLINE) {
        params.push(53);
    }
    params
}

// ── HTML export (signature — implementation in bd-2vr05.5.3) ─────────

/// Export terminal content as styled HTML.
///
/// Produces a `<pre>` block with `<span>` elements carrying inline CSS
/// or class attributes. Hyperlinks are rendered as `<a>` tags when
/// enabled.
///
/// # Note
///
/// Full implementation is tracked in bd-2vr05.5.3. This function
/// currently delegates to a minimal stub that emits escaped plain text
/// wrapped in a `<pre>` element.
#[must_use]
pub fn export_html(ctx: &ExportContext<'_>, opts: &HtmlExportOptions) -> String {
    // Minimal stub: emit escaped plain text in <pre>.
    // Full HTML serializer with styles/hyperlinks in bd-2vr05.5.3.
    let text_opts = TextExportOptions {
        range: opts.range.clone(),
        line_ending: opts.line_ending,
        trim_trailing: opts.trim_trailing,
        join_soft_wraps: false,
        include_combining: true,
    };
    let text = export_text(ctx, &text_opts);
    let escaped = html_escape(&text);

    let mut out = String::with_capacity(escaped.len() + 200);
    write!(
        out,
        "<pre class=\"{}\" style=\"font-family:{};font-size:{};\">",
        opts.class_prefix, opts.font_family, opts.font_size,
    )
    .unwrap();
    out.push_str(&escaped);
    out.push_str("</pre>");
    out
}

/// Escape special HTML characters in text.
///
/// Public so downstream HTML export implementations (bd-2vr05.5.3) can reuse it.
#[must_use]
pub fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{Cell, HyperlinkRegistry, SgrAttrs};
    use crate::grid::Grid;
    use crate::scrollback::Scrollback;

    // ── Helpers ──────────────────────────────────────────────────────

    fn make_grid(cols: u16, lines: &[&str]) -> Grid {
        let rows = lines.len() as u16;
        let mut g = Grid::new(cols, rows);
        for (r, text) in lines.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                if c >= cols as usize {
                    break;
                }
                g.cell_mut(r as u16, c as u16).unwrap().set_content(ch, 1);
            }
        }
        g
    }

    fn make_scrollback(lines: &[(&str, bool)]) -> Scrollback {
        let mut sb = Scrollback::new(64);
        for (text, wrapped) in lines {
            let cells: Vec<Cell> = text.chars().map(Cell::new).collect();
            sb.push_row(&cells, *wrapped);
        }
        sb
    }

    fn default_ctx<'a>(
        grid: &'a Grid,
        scrollback: &'a Scrollback,
        hyperlinks: &'a HyperlinkRegistry,
    ) -> ExportContext<'a> {
        ExportContext::new(grid, scrollback, hyperlinks)
    }

    // ── ExportRange + resolve_rows tests ─────────────────────────────

    #[test]
    fn resolve_viewport_only() {
        let grid = make_grid(5, &["hello", "world"]);
        let sb = Scrollback::new(0);
        let rows = resolve_rows(&grid, &sb, &ExportRange::Viewport);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells.len(), 5);
        assert_eq!(rows[0].cells[0].content(), 'h');
        assert!(!rows[0].is_soft_wrapped);
    }

    #[test]
    fn resolve_scrollback_only() {
        let grid = Grid::new(5, 1);
        let sb = make_scrollback(&[("aaa", false), ("bbb", true), ("ccc", false)]);
        let rows = resolve_rows(&grid, &sb, &ExportRange::ScrollbackOnly);
        assert_eq!(rows.len(), 3);
        // "aaa" is soft-wrapped because next line has wrapped=true.
        assert!(rows[0].is_soft_wrapped);
        // "bbb" is NOT soft-wrapped because next line has wrapped=false.
        assert!(!rows[1].is_soft_wrapped);
        assert!(!rows[2].is_soft_wrapped);
    }

    #[test]
    fn resolve_full_includes_both() {
        let grid = make_grid(5, &["grid"]);
        let sb = make_scrollback(&[("sb", false)]);
        let rows = resolve_rows(&grid, &sb, &ExportRange::Full);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells[0].content(), 's');
        assert_eq!(rows[1].cells[0].content(), 'g');
    }

    #[test]
    fn resolve_lines_range_across_boundary() {
        let grid = make_grid(5, &["vp0", "vp1"]);
        let sb = make_scrollback(&[("sb0", false), ("sb1", false)]);
        // Lines 1..3 = sb1, vp0
        let rows = resolve_rows(&grid, &sb, &ExportRange::Lines(1..3));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells[0].content(), 's'); // sb1
        assert_eq!(rows[1].cells[0].content(), 'v'); // vp0
    }

    #[test]
    fn resolve_lines_empty_range() {
        let grid = make_grid(5, &["x"]);
        let sb = Scrollback::new(0);
        let rows = resolve_rows(&grid, &sb, &ExportRange::Lines(0..0));
        assert!(rows.is_empty());
    }

    #[test]
    fn resolve_lines_beyond_bounds() {
        let grid = make_grid(5, &["x"]);
        let sb = Scrollback::new(0);
        let rows = resolve_rows(&grid, &sb, &ExportRange::Lines(5..10));
        assert!(rows.is_empty());
    }

    // ── Text export tests ────────────────────────────────────────────

    #[test]
    fn text_export_viewport_basic() {
        let grid = make_grid(10, &["hello", "world"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let text = export_text(&ctx, &TextExportOptions::default());
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn text_export_trims_trailing_spaces() {
        let grid = make_grid(10, &["hi"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let text = export_text(&ctx, &TextExportOptions::default());
        assert_eq!(text, "hi");
    }

    #[test]
    fn text_export_no_trim() {
        let grid = make_grid(5, &["ab"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            trim_trailing: false,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "ab   "); // 5 cols, "ab" + 3 spaces
    }

    #[test]
    fn text_export_crlf_line_endings() {
        let grid = make_grid(5, &["aa", "bb"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            line_ending: LineEnding::CrLf,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "aa\r\nbb");
    }

    #[test]
    fn text_export_joins_soft_wraps() {
        let grid = Grid::new(5, 1);
        let sb = make_scrollback(&[("hello", false), ("world", true)]);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            range: ExportRange::ScrollbackOnly,
            join_soft_wraps: true,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "helloworld");
    }

    #[test]
    fn text_export_no_join_soft_wraps() {
        let grid = Grid::new(5, 1);
        let sb = make_scrollback(&[("hello", false), ("world", true)]);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            range: ExportRange::ScrollbackOnly,
            join_soft_wraps: false,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn text_export_full_range() {
        let grid = make_grid(10, &["viewport"]);
        let sb = make_scrollback(&[("history", false)]);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            range: ExportRange::Full,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "history\nviewport");
    }

    #[test]
    fn text_export_empty_grid() {
        let grid = Grid::new(0, 0);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let text = export_text(&ctx, &TextExportOptions::default());
        assert_eq!(text, "");
    }

    #[test]
    fn text_export_wide_char_appears_once() {
        let mut grid = Grid::new(10, 1);
        let (lead, cont) = Cell::wide('\u{4E2D}', SgrAttrs::default()); // '中'
        *grid.cell_mut(0, 0).unwrap() = lead;
        *grid.cell_mut(0, 1).unwrap() = cont;
        grid.cell_mut(0, 2).unwrap().set_content('x', 1);

        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let text = export_text(&ctx, &TextExportOptions::default());
        assert_eq!(text, "中x");
    }

    #[test]
    fn text_export_combining_marks() {
        let mut grid = Grid::new(10, 1);
        grid.cell_mut(0, 0).unwrap().set_content('e', 1);
        grid.cell_mut(0, 0).unwrap().push_combining('\u{0301}');

        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            include_combining: true,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "e\u{0301}");

        let opts_no_combining = TextExportOptions {
            include_combining: false,
            ..Default::default()
        };
        let text = export_text(&ctx, &opts_no_combining);
        assert_eq!(text, "e");
    }

    #[test]
    fn text_export_lines_range() {
        let grid = make_grid(5, &["vp"]);
        let sb = make_scrollback(&[("sb0", false), ("sb1", false)]);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            range: ExportRange::Lines(1..3),
            ..Default::default()
        };
        let text = export_text(&ctx, &opts);
        assert_eq!(text, "sb1\nvp");
    }

    // ── ANSI export stub tests ───────────────────────────────────────

    #[test]
    fn ansi_export_stub_includes_reset() {
        let grid = make_grid(5, &["hi"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let result = export_ansi(&ctx, &AnsiExportOptions::default());
        assert!(result.ends_with("\x1b[0m"));
        assert!(result.contains("hi"));
    }

    #[test]
    fn ansi_export_stub_no_reset() {
        let grid = make_grid(5, &["hi"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = AnsiExportOptions {
            reset_at_end: false,
            ..Default::default()
        };
        let result = export_ansi(&ctx, &opts);
        assert!(!result.ends_with("\x1b[0m"));
    }

    // ── HTML export stub tests ───────────────────────────────────────

    #[test]
    fn html_export_stub_wraps_in_pre() {
        let grid = make_grid(5, &["hi"]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let result = export_html(&ctx, &HtmlExportOptions::default());
        assert!(result.starts_with("<pre"));
        assert!(result.ends_with("</pre>"));
        assert!(result.contains("hi"));
    }

    #[test]
    fn html_export_escapes_special_chars() {
        let grid = make_grid(20, &["<b>test</b> & \"ok\""]);
        let sb = Scrollback::new(0);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let result = export_html(&ctx, &HtmlExportOptions::default());
        assert!(result.contains("&lt;b&gt;"));
        assert!(result.contains("&amp;"));
        assert!(result.contains("&quot;"));
    }

    // ── Helper function tests ────────────────────────────────────────

    #[test]
    fn html_escape_special_chars() {
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#39;");
        assert_eq!(html_escape("hello"), "hello");
        assert_eq!(html_escape(""), "");
    }

    #[test]
    fn color_to_sgr_truecolor() {
        assert_eq!(
            color_to_sgr(Color::Rgb(255, 0, 128), 38, ColorDepth::TrueColor),
            Some("38;2;255;0;128".into())
        );
        assert_eq!(
            color_to_sgr(Color::Named(1), 38, ColorDepth::TrueColor),
            Some("38;5;1".into())
        );
        assert_eq!(
            color_to_sgr(Color::Indexed(200), 48, ColorDepth::TrueColor),
            Some("48;5;200".into())
        );
        assert_eq!(
            color_to_sgr(Color::Default, 38, ColorDepth::TrueColor),
            None
        );
    }

    #[test]
    fn color_to_sgr_named16() {
        assert_eq!(
            color_to_sgr(Color::Named(1), 38, ColorDepth::Named16),
            Some("31".into()) // 30 + 1
        );
        assert_eq!(
            color_to_sgr(Color::Named(9), 38, ColorDepth::Named16),
            Some("91".into()) // 90 + (9-8)
        );
        assert_eq!(
            color_to_sgr(Color::Named(0), 48, ColorDepth::Named16),
            Some("40".into())
        );
        // RGB downgraded to None in 16-color mode.
        assert_eq!(
            color_to_sgr(Color::Rgb(255, 0, 0), 38, ColorDepth::Named16),
            None
        );
    }

    #[test]
    fn color_to_sgr_no_color() {
        assert_eq!(
            color_to_sgr(Color::Rgb(255, 0, 0), 38, ColorDepth::NoColor),
            None
        );
        assert_eq!(color_to_sgr(Color::Named(1), 38, ColorDepth::NoColor), None);
    }

    #[test]
    fn color_to_sgr_indexed256() {
        assert_eq!(
            color_to_sgr(Color::Indexed(42), 38, ColorDepth::Indexed256),
            Some("38;5;42".into())
        );
        // RGB approximated to 256 palette.
        let result = color_to_sgr(Color::Rgb(255, 0, 0), 38, ColorDepth::Indexed256);
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("38;5;"));
    }

    #[test]
    fn rgb_to_256_grayscale() {
        // Pure black.
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        // Pure white.
        assert_eq!(rgb_to_256(255, 255, 255), 231);
        // Mid-gray should be in the grayscale ramp.
        let idx = rgb_to_256(128, 128, 128);
        assert!(idx >= 232);
    }

    #[test]
    fn rgb_to_256_color_cube() {
        // Pure red should map to the red corner of the 6x6x6 cube.
        let idx = rgb_to_256(255, 0, 0);
        assert_eq!(idx, 16 + 36 * 5); // 196
    }

    #[test]
    fn sgr_flags_to_params_empty() {
        assert!(sgr_flags_to_params(SgrFlags::empty()).is_empty());
    }

    #[test]
    fn sgr_flags_to_params_all_flags() {
        let flags = SgrFlags::BOLD
            | SgrFlags::DIM
            | SgrFlags::ITALIC
            | SgrFlags::UNDERLINE
            | SgrFlags::BLINK
            | SgrFlags::INVERSE
            | SgrFlags::HIDDEN
            | SgrFlags::STRIKETHROUGH
            | SgrFlags::DOUBLE_UNDERLINE
            | SgrFlags::OVERLINE;
        let params = sgr_flags_to_params(flags);
        assert_eq!(params, vec![1, 2, 3, 4, 5, 7, 8, 9, 21, 53]);
    }

    #[test]
    fn sgr_flags_to_params_single() {
        assert_eq!(sgr_flags_to_params(SgrFlags::BOLD), vec![1]);
        assert_eq!(sgr_flags_to_params(SgrFlags::ITALIC), vec![3]);
        assert_eq!(sgr_flags_to_params(SgrFlags::STRIKETHROUGH), vec![9]);
    }

    // ── LineEnding tests ─────────────────────────────────────────────

    #[test]
    fn line_ending_as_str() {
        assert_eq!(LineEnding::Lf.as_str(), "\n");
        assert_eq!(LineEnding::CrLf.as_str(), "\r\n");
    }

    #[test]
    fn line_ending_default_is_lf() {
        assert_eq!(LineEnding::default(), LineEnding::Lf);
    }

    // ── Default option tests ─────────────────────────────────────────

    #[test]
    fn ansi_options_default() {
        let opts = AnsiExportOptions::default();
        assert_eq!(opts.range, ExportRange::Viewport);
        assert_eq!(opts.color_depth, ColorDepth::TrueColor);
        assert!(opts.trim_trailing);
        assert!(opts.reset_at_end);
        assert!(opts.join_soft_wraps);
    }

    #[test]
    fn html_options_default() {
        let opts = HtmlExportOptions::default();
        assert_eq!(opts.range, ExportRange::Viewport);
        assert!(opts.inline_styles);
        assert_eq!(opts.class_prefix, "ft");
        assert!(opts.render_hyperlinks);
        assert!(opts.trim_trailing);
    }

    #[test]
    fn text_options_default() {
        let opts = TextExportOptions::default();
        assert_eq!(opts.range, ExportRange::Viewport);
        assert!(opts.trim_trailing);
        assert!(opts.join_soft_wraps);
        assert!(opts.include_combining);
    }

    // ── Determinism test ─────────────────────────────────────────────

    #[test]
    fn text_export_is_deterministic() {
        let grid = make_grid(10, &["hello", "world"]);
        let sb = make_scrollback(&[("history", false)]);
        let reg = HyperlinkRegistry::new();
        let ctx = default_ctx(&grid, &sb, &reg);

        let opts = TextExportOptions {
            range: ExportRange::Full,
            ..Default::default()
        };

        let a = export_text(&ctx, &opts);
        let b = export_text(&ctx, &opts);
        assert_eq!(a, b, "export_text must be deterministic for fixed inputs");
    }

    #[test]
    fn row_cells_to_text_basic() {
        let cells: Vec<Cell> = "hello".chars().map(Cell::new).collect();
        assert_eq!(row_cells_to_text(&cells, true, true), "hello");
    }

    #[test]
    fn row_cells_to_text_trims_trailing() {
        let mut cells: Vec<Cell> = "hi".chars().map(Cell::new).collect();
        cells.push(Cell::default()); // space
        cells.push(Cell::default()); // space
        assert_eq!(row_cells_to_text(&cells, true, true), "hi");
        assert_eq!(row_cells_to_text(&cells, true, false), "hi  ");
    }

    #[test]
    fn row_cells_to_text_wide_char() {
        let (lead, cont) = Cell::wide('中', SgrAttrs::default());
        let mut cells = vec![lead, cont];
        cells.push(Cell::new('x'));
        assert_eq!(row_cells_to_text(&cells, true, true), "中x");
    }

    #[test]
    fn row_cells_to_text_combining() {
        let mut cell = Cell::new('e');
        cell.push_combining('\u{0301}');
        let cells = vec![cell];
        assert_eq!(row_cells_to_text(&cells, true, true), "e\u{0301}");
        assert_eq!(row_cells_to_text(&cells, false, true), "e");
    }
}
