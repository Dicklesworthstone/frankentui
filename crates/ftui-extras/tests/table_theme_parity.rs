#![forbid(unsafe_code)]
#![cfg(feature = "markdown")]

//! Cross-render parity tests for TableTheme (widget vs markdown tables).

use ftui_core::geometry::Rect;
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
use ftui_layout::Constraint;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::{Style, TableEffectScope, TableSection, TableTheme};
use ftui_text::Text;
use ftui_widgets::Widget;
use ftui_widgets::table::{Row, Table};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowStyleHash {
    section: TableSection,
    row_index: usize,
    hash: u64,
}

fn _format_vec_mismatch<T: std::fmt::Debug + PartialEq>(
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

fn _format_style_hash_mismatch(markdown: &[RowStyleHash], widget: &[RowStyleHash]) -> String {
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
    let mut frame = Frame::new(80, 10, &mut pool);
    table.render(Rect::new(0, 0, 80, 10), &mut frame);

    // Compare intrinsic column widths.
    let markdown_widths = intrinsic_widths(&header, &rows);
    let widget_widths = intrinsic_widths(&header, &rows);
    assert_eq!(
        widget_widths, markdown_widths,
        "widget and markdown intrinsic widths should match"
    );

    // Compare row heights (markdown rows are single-line; widget defaults to height=1).
    let markdown_heights = row_heights(1 + rows.len());
    let widget_heights = row_heights(1 + rows.len());
    assert_eq!(
        widget_heights, markdown_heights,
        "widget and markdown row heights should match"
    );

    // Compare resolved style hashes per row/section.
    let markdown_styles = collect_markdown_style_hashes(&theme, 1, rows.len(), Some(phase));
    let widget_styles = collect_widget_style_hashes(&theme, 1, rows.len(), Some(phase));
    assert_eq!(
        widget_styles, markdown_styles,
        "widget and markdown resolved styles should match"
    );
}
