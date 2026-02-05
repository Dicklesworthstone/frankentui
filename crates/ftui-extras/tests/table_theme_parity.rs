#![forbid(unsafe_code)]
#![cfg(feature = "markdown")]

//! Cross-render parity tests for TableTheme (widget vs markdown tables).

use ftui_core::geometry::Rect;
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
use ftui_layout::Constraint;
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::{Style, TableEffectScope, TableSection, TableTheme};
use ftui_text::{Line, Text};
use ftui_widgets::Widget;
use ftui_widgets::borders::BorderSet;
use ftui_widgets::table::{Row, Table};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowStyleHash {
    section: TableSection,
    row_index: usize,
    hash: u64,
}

fn format_vec_mismatch<T: std::fmt::Debug + PartialEq>(
    label: &str,
    left: &[T],
    right: &[T],
) -> String {
    let mut lines = Vec::new();
    let max_len = left.len().max(right.len());
    for idx in 0..max_len {
        let l = left.get(idx);
        let r = right.get(idx);
        if l != r {
            lines.push(format!("{label}[{idx}]: left={l:?}, right={r:?}"));
        }
    }
    if lines.is_empty() {
        format!(
            "{label}: no mismatches (len left={}, len right={})",
            left.len(),
            right.len()
        )
    } else {
        lines.join("\n")
    }
}

fn format_style_hash_mismatch(markdown: &[RowStyleHash], widget: &[RowStyleHash]) -> String {
    let mut lines = Vec::new();
    let max_len = markdown.len().max(widget.len());
    for idx in 0..max_len {
        let left = markdown.get(idx);
        let right = widget.get(idx);
        if left != right {
            match (left, right) {
                (Some(left), Some(right)) => {
                    lines.push(format!(
                        "{:?} row {}: markdown=0x{:016x}, widget=0x{:016x}",
                        left.section, left.row_index, left.hash, right.hash
                    ));
                }
                (Some(left), None) => {
                    lines.push(format!(
                        "{:?} row {}: markdown=0x{:016x}, widget=<missing>",
                        left.section, left.row_index, left.hash
                    ));
                }
                (None, Some(right)) => {
                    lines.push(format!(
                        "{:?} row {}: markdown=<missing>, widget=0x{:016x}",
                        right.section, right.row_index, right.hash
                    ));
                }
                (None, None) => {}
            }
        }
    }
    if lines.is_empty() {
        format!(
            "style hashes match (len markdown={}, len widget={})",
            markdown.len(),
            widget.len()
        )
    } else {
        lines.join("\n")
    }
}

fn build_markdown_table(header: &[&str], rows: &[Vec<&str>]) -> String {
    let mut out = String::new();
    out.push('|');
    for cell in header {
        out.push(' ');
        out.push_str(cell);
        out.push(' ');
        out.push('|');
    }
    out.push('\n');
    out.push('|');
    for _ in header {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        out.push('|');
        for cell in row {
            out.push(' ');
            out.push_str(cell);
            out.push(' ');
            out.push('|');
        }
        out.push('\n');
    }
    out
}

fn line_to_string(line: &Line) -> String {
    let mut out = String::new();
    for span in line.spans() {
        out.push_str(&span.content);
    }
    out
}

fn parse_border_widths(border_line: &str) -> Vec<u16> {
    let mut widths = Vec::new();
    let mut run_len = 0usize;
    let mut iter = border_line.chars();
    if iter.next().is_none() {
        return widths;
    }
    for ch in iter {
        if ch == '┬' || ch == '┐' {
            let width = run_len.saturating_sub(2).min(u16::MAX as usize) as u16;
            widths.push(width);
            run_len = 0;
            if ch == '┐' {
                break;
            }
        } else {
            run_len = run_len.saturating_add(1);
        }
    }
    widths
}

fn extract_markdown_column_widths(rendered: &Text) -> Vec<u16> {
    let border_line = rendered
        .lines()
        .iter()
        .map(line_to_string)
        .find(|line| line.starts_with('┌'))
        .expect("markdown table border line missing");
    parse_border_widths(&border_line)
}

fn extract_markdown_row_count(rendered: &Text) -> usize {
    rendered
        .lines()
        .iter()
        .map(line_to_string)
        .filter(|line| line.starts_with('│'))
        .count()
}

fn extract_widget_column_widths(buf: &Buffer, divider: char) -> Vec<u16> {
    let y = 0u16;
    let mut dividers = Vec::new();
    for x in 0..buf.width() {
        let ch = buf.get(x, y).and_then(|cell| cell.content.as_char());
        if ch == Some(divider) {
            dividers.push(x);
        }
    }

    let mut widths = Vec::new();
    let mut start = 0u16;
    for pos in dividers {
        widths.push(pos.saturating_sub(start));
        start = pos.saturating_add(1);
    }
    if start < buf.width() {
        widths.push(buf.width().saturating_sub(start));
    }
    widths
}

fn cell_width(text: &str) -> u16 {
    Text::raw(text).width().min(u16::MAX as usize) as u16
}

fn intrinsic_widths(header: &[&str], rows: &[Vec<&str>]) -> Vec<u16> {
    let col_count = header
        .len()
        .max(rows.iter().map(|row| row.len()).max().unwrap_or(0));
    let mut widths = vec![0u16; col_count];
    for (idx, cell) in header.iter().enumerate() {
        widths[idx] = widths[idx].max(cell_width(cell));
    }
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell_width(cell));
        }
    }
    widths
}

fn row_heights(row_count: usize) -> Vec<u16> {
    vec![1u16; row_count]
}

fn style_hash(style: Style) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    fn mix_bytes(hash: u64, bytes: &[u8]) -> u64 {
        let mut h = hash;
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        h
    }

    let mut hash = FNV_OFFSET;
    hash = mix_bytes(hash, &[style.fg.is_some() as u8]);
    if let Some(color) = style.fg {
        hash = mix_bytes(hash, &color.0.to_le_bytes());
    }
    hash = mix_bytes(hash, &[style.bg.is_some() as u8]);
    if let Some(color) = style.bg {
        hash = mix_bytes(hash, &color.0.to_le_bytes());
    }
    hash = mix_bytes(hash, &[style.attrs.is_some() as u8]);
    if let Some(attrs) = style.attrs {
        hash = mix_bytes(hash, &attrs.0.to_le_bytes());
    }
    hash = mix_bytes(hash, &[style.underline_color.is_some() as u8]);
    if let Some(color) = style.underline_color {
        hash = mix_bytes(hash, &color.0.to_le_bytes());
    }
    hash
}

fn resolve_markdown_style(
    theme: &TableTheme,
    section: TableSection,
    row_index: usize,
    phase: Option<f32>,
    is_header: bool,
) -> Style {
    let base = if is_header {
        theme.header
    } else if row_index.is_multiple_of(2) {
        theme.row
    } else {
        theme.row_alt
    };

    if phase.is_some() && theme.effects.is_empty() {
        return base;
    }

    if let Some(phase) = phase {
        let resolver = theme.effect_resolver();
        let resolved = resolver.resolve(base, TableEffectScope::section(section), phase);
        resolver.resolve(resolved, TableEffectScope::row(section, row_index), phase)
    } else {
        base
    }
}

fn resolve_widget_style(
    theme: &TableTheme,
    section: TableSection,
    row_index: usize,
    phase: Option<f32>,
    is_header: bool,
) -> Style {
    let base = if is_header {
        theme.header
    } else if row_index.is_multiple_of(2) {
        theme.row
    } else {
        theme.row_alt
    };

    if phase.is_some() && theme.effects.is_empty() {
        return base;
    }

    if let Some(phase) = phase {
        let resolver = theme.effect_resolver();
        let scope = if is_header {
            TableEffectScope::section(section)
        } else {
            TableEffectScope::row(section, row_index)
        };
        resolver.resolve(base, scope, phase)
    } else {
        base
    }
}

fn collect_markdown_style_hashes(
    theme: &TableTheme,
    header_rows: usize,
    body_rows: usize,
    phase: Option<f32>,
) -> Vec<RowStyleHash> {
    let mut out = Vec::with_capacity(header_rows + body_rows);
    for row_index in 0..header_rows {
        let style = resolve_markdown_style(theme, TableSection::Header, row_index, phase, true);
        out.push(RowStyleHash {
            section: TableSection::Header,
            row_index,
            hash: style_hash(style),
        });
    }
    for row_index in 0..body_rows {
        let style = resolve_markdown_style(theme, TableSection::Body, row_index, phase, false);
        out.push(RowStyleHash {
            section: TableSection::Body,
            row_index,
            hash: style_hash(style),
        });
    }
    out
}

fn collect_widget_style_hashes(
    theme: &TableTheme,
    header_rows: usize,
    body_rows: usize,
    phase: Option<f32>,
) -> Vec<RowStyleHash> {
    let mut out = Vec::with_capacity(header_rows + body_rows);
    for row_index in 0..header_rows {
        let style = resolve_widget_style(theme, TableSection::Header, row_index, phase, true);
        out.push(RowStyleHash {
            section: TableSection::Header,
            row_index,
            hash: style_hash(style),
        });
    }
    for row_index in 0..body_rows {
        let style = resolve_widget_style(theme, TableSection::Body, row_index, phase, false);
        out.push(RowStyleHash {
            section: TableSection::Body,
            row_index,
            hash: style_hash(style),
        });
    }
    out
}

#[test]
fn table_theme_parity_widget_vs_markdown() {
    let header = ["Name", "Role", "Status"];
    let rows = vec![
        vec!["Ada", "Compiler wizard", "Active"],
        vec!["Linus", "Kernel architect", "Active"],
        vec!["Grace Hopper", "COBOL pioneer", "Retired"],
        vec!["Ken", "UNIX co-creator", "Legend"],
    ];

    let theme = TableTheme::aurora();
    let phase = 0.25;

    // Render markdown table (ensures markdown path is exercised).
    let markdown = build_markdown_table(&header, &rows);
    let markdown_theme = MarkdownTheme {
        table_theme: theme.clone(),
        ..Default::default()
    };
    let renderer = MarkdownRenderer::new(markdown_theme).table_effect_phase(phase);
    let rendered = renderer.render(&markdown);
    assert!(
        !rendered.is_empty(),
        "markdown render should produce output"
    );

    // Render widget table (ensures widget path is exercised).
    let widget_rows: Vec<Row> = rows
        .iter()
        .map(|row| Row::new(row.iter().copied()))
        .collect();
    let header_row = Row::new(header.iter().copied());
    let constraints = header
        .iter()
        .map(|_| Constraint::FitContent)
        .collect::<Vec<_>>();
    let table = Table::new(widget_rows.clone(), constraints)
        .header(header_row)
        .theme(theme.clone())
        .theme_phase(phase)
        .column_spacing(1);

    let mut pool = GraphemePool::new();
    let intrinsic = intrinsic_widths(&header, &rows);
    let col_count = intrinsic.len().max(1);
    let column_spacing = 1u16;
    let table_width = intrinsic
        .iter()
        .fold(0u16, |acc, w| acc.saturating_add(*w))
        .saturating_add(column_spacing.saturating_mul(col_count.saturating_sub(1) as u16));
    let table_height = (1 + rows.len()).min(u16::MAX as usize) as u16;
    let mut frame = Frame::new(table_width, table_height, &mut pool);
    table.render(Rect::new(0, 0, table_width, table_height), &mut frame);

    // Compare column widths derived from renderer outputs.
    let markdown_widths = extract_markdown_column_widths(&rendered);
    let widget_widths = extract_widget_column_widths(&frame.buffer, BorderSet::SQUARE.vertical);
    assert_eq!(
        widget_widths,
        markdown_widths,
        "column widths mismatch:\n{}",
        format_vec_mismatch("column_widths", &markdown_widths, &widget_widths)
    );

    // Compare row heights (markdown rows are single-line; widget defaults to height=1).
    let markdown_row_count = extract_markdown_row_count(&rendered);
    assert_eq!(
        markdown_row_count,
        1 + rows.len(),
        "markdown row count mismatch: expected {}, got {}",
        1 + rows.len(),
        markdown_row_count
    );
    let markdown_heights = row_heights(markdown_row_count);
    let widget_heights = row_heights(1 + rows.len());
    assert_eq!(
        widget_heights,
        markdown_heights,
        "row heights mismatch:\n{}",
        format_vec_mismatch("row_heights", &markdown_heights, &widget_heights)
    );

    // Compare resolved style hashes per row/section.
    let markdown_styles = collect_markdown_style_hashes(&theme, 1, rows.len(), Some(phase));
    let widget_styles = collect_widget_style_hashes(&theme, 1, rows.len(), Some(phase));
    assert_eq!(
        widget_styles,
        markdown_styles,
        "style hash mismatch:\n{}",
        format_style_hash_mismatch(&markdown_styles, &widget_styles)
    );
}
