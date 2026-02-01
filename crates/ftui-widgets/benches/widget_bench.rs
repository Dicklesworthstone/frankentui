//! Benchmarks for widget rendering (bd-19x)
//!
//! Run with: cargo bench -p ftui-widgets

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_layout::Constraint;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::Style;
use ftui_text::Text;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};
use std::hint::black_box;

// ============================================================================
// Block widget
// ============================================================================

fn bench_block_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/block");

    let block_plain = Block::new();
    let block_bordered = Block::new().borders(Borders::ALL).title("Title");

    for (w, h) in [(40, 10), (80, 24), (200, 60)] {
        let area = Rect::from_size(w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);

        group.bench_with_input(
            BenchmarkId::new("plain", format!("{w}x{h}")),
            &(),
            |b, _| {
                b.iter(|| {
                    frame.buffer.clear();
                    block_plain.render(area, &mut frame);
                    black_box(&frame.buffer);
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("bordered", format!("{w}x{h}")),
            &(),
            |b, _| {
                b.iter(|| {
                    frame.buffer.clear();
                    block_bordered.render(area, &mut frame);
                    black_box(&frame.buffer);
                })
            },
        );
    }

    group.finish();
}

// ============================================================================
// Paragraph widget
// ============================================================================

fn make_paragraph_text(chars: usize) -> Text {
    let content: String = "The quick brown fox jumps over the lazy dog. "
        .chars()
        .cycle()
        .take(chars)
        .collect();
    Text::raw(content)
}

fn bench_paragraph_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/paragraph");

    for (chars, label) in [(50, "50ch"), (200, "200ch"), (1000, "1000ch")] {
        let text = make_paragraph_text(chars);
        let para = Paragraph::new(text);
        let area = Rect::from_size(80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        group.bench_with_input(BenchmarkId::new("no_wrap", label), &para, |b, para| {
            b.iter(|| {
                frame.buffer.clear();
                para.render(area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

fn bench_paragraph_wrapped(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/paragraph_wrap");

    for (chars, label) in [(200, "200ch"), (1000, "1000ch"), (5000, "5000ch")] {
        let text = make_paragraph_text(chars);
        let para = Paragraph::new(text).wrap(ftui_text::WrapMode::Word);
        let area = Rect::from_size(80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        group.bench_with_input(BenchmarkId::new("word_wrap", label), &para, |b, para| {
            b.iter(|| {
                frame.buffer.clear();
                para.render(area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

// ============================================================================
// Table widget
// ============================================================================

fn make_table(row_count: usize, col_count: usize) -> (Table<'static>, Vec<Constraint>) {
    let widths: Vec<Constraint> = (0..col_count)
        .map(|_| Constraint::Percentage(100.0 / col_count as f32))
        .collect();

    let rows: Vec<Row> = (0..row_count)
        .map(|r| {
            let cells: Vec<String> = (0..col_count).map(|col| format!("R{r}C{col}")).collect();
            Row::new(cells)
        })
        .collect();

    let header_cells: Vec<String> = (0..col_count).map(|c| format!("Col {c}")).collect();
    let header = Row::new(header_cells).style(Style::new().fg(PackedRgba::rgb(255, 255, 0)));

    let table = Table::new(rows, widths.clone())
        .header(header)
        .block(Block::new().borders(Borders::ALL).title("Data"));

    (table, widths)
}

fn bench_table_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/table");

    for (rows, cols, label) in [
        (10, 3, "10x3"),
        (50, 5, "50x5"),
        (100, 3, "100x3"),
        (100, 8, "100x8"),
    ] {
        let (table, _) = make_table(rows, cols);
        let area = Rect::from_size(120, 40);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);

        group.bench_with_input(BenchmarkId::new("render", label), &table, |b, table| {
            b.iter(|| {
                frame.buffer.clear();
                table.render(area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_block_render,
    bench_paragraph_render,
    bench_paragraph_wrapped,
    bench_table_render,
);

criterion_main!(benches);
