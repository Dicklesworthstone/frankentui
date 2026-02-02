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
use ftui_widgets::log_ring::LogRing;
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};
use ftui_widgets::virtualized::Virtualized;
use ftui_widgets::StatefulWidget;
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
                Widget::render(table, area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

// ============================================================================
// Virtualization benchmarks (bd-uo6v)
// ============================================================================

fn bench_log_ring_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_ring_push");

    for (capacity, label) in [(1_000, "1K"), (10_000, "10K"), (100_000, "100K")] {
        let mut ring: LogRing<String> = LogRing::new(capacity);

        group.bench_function(BenchmarkId::new("push", label), |b| {
            b.iter(|| {
                ring.push(black_box("Log line: operation completed successfully".to_string()));
            })
        });
    }

    group.finish();
}

fn bench_log_ring_push_at_capacity(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_ring_push_full");

    for (capacity, label) in [(1_000, "1K"), (10_000, "10K")] {
        // Pre-fill to capacity
        let mut ring: LogRing<String> = LogRing::new(capacity);
        for i in 0..capacity {
            ring.push(format!("Line {}", i));
        }

        group.bench_function(BenchmarkId::new("push_evict", label), |b| {
            b.iter(|| {
                ring.push(black_box("New line replacing oldest".to_string()));
            })
        });
    }

    group.finish();
}

fn bench_virtualized_scroll(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/scroll");

    for (count, label) in [(1_000, "1K"), (10_000, "10K"), (100_000, "100K")] {
        let mut virt: Virtualized<i32> = Virtualized::new(count);
        for i in 0..count {
            virt.push(i as i32);
        }
        virt.set_visible_count(24);

        group.bench_function(BenchmarkId::new("scroll_one", label), |b| {
            b.iter(|| {
                virt.scroll(black_box(1));
                black_box(virt.scroll_offset());
            })
        });

        group.bench_function(BenchmarkId::new("page_down", label), |b| {
            b.iter(|| {
                virt.page_down();
                black_box(virt.scroll_offset());
            })
        });

        group.bench_function(BenchmarkId::new("visible_range", label), |b| {
            b.iter(|| {
                black_box(virt.visible_range(24));
            })
        });
    }

    group.finish();
}

fn bench_log_viewer_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_viewer_render");

    for (count, label) in [(100, "100"), (1_000, "1K"), (10_000, "10K"), (100_000, "100K")] {
        let mut viewer = LogViewer::new(count);
        for i in 0..count {
            viewer.push(format!("[{:>6}] INFO  app::module: Processing request #{}", i, i));
        }

        let area = Rect::from_size(80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let mut state = LogViewerState::default();

        group.bench_function(BenchmarkId::new("render", label), |b| {
            b.iter(|| {
                frame.buffer.clear();
                viewer.render(area, &mut frame, &mut state);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

fn bench_log_viewer_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_viewer_search");

    for (count, label) in [(1_000, "1K"), (10_000, "10K")] {
        let mut viewer = LogViewer::new(count);
        for i in 0..count {
            if i % 100 == 0 {
                viewer.push(format!("[{:>6}] ERROR app::handler: Request failed", i));
            } else {
                viewer.push(format!("[{:>6}] INFO  app::handler: Request OK", i));
            }
        }

        group.bench_function(BenchmarkId::new("search", label), |b| {
            b.iter(|| {
                black_box(viewer.search(black_box("ERROR")));
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
    bench_log_ring_push,
    bench_log_ring_push_at_capacity,
    bench_virtualized_scroll,
    bench_log_viewer_render,
    bench_log_viewer_search,
);

criterion_main!(benches);
