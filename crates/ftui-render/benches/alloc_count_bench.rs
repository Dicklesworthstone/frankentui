#![forbid(unsafe_code)]

//! Per-frame heap allocation counting benchmark for ASCII and Unicode dashboard scenes
//! (`bd-g00-root-epic-ewths.31.2`, `bd-g00-root-epic-ewths.31.3`).
//!
//! Run with:
//! `cargo bench -p ftui-render --bench alloc_count_bench --features alloc-count`
//!
//! Uses `stats_alloc::StatsAlloc<std::alloc::System>` (`#![forbid(unsafe_code)]`)
//! to count heap allocations across `view + diff + present` after a warm-up frame,
//! both with `FrameArena` attached (`arena: true`) and without (`arena: false`),
//! and writes `target/regression-gate/alloc_report.json`.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use alloc_counter::{
    assert_counter_counts_box_alloc_and_free, assert_counter_is_zero_for_stack_only_work,
    measure_allocations,
};
use criterion::{Criterion, criterion_group};
use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_layout::{Constraint, Flex};
use ftui_render::arena::FrameArena;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::Presenter;
use ftui_text::wrap::WrapMode;
use ftui_widgets::block::Block;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::{StatefulWidget, Widget};
use serde::Serialize;
use std::hint::black_box;
use std::path::PathBuf;

const COLS: u16 = 80;
const ROWS: u16 = 24;

#[derive(Debug, Clone, Serialize)]
struct AllocReportEntry {
    scene: String,
    arena: bool,
    allocs_per_frame: usize,
    raw_allocs_per_frame: usize,
    baseline_allocs: usize,
    bytes_per_frame: usize,
    arena_bytes: usize,
}

struct DashboardScene {
    warmup_paragraph: Paragraph<'static>,
    measured_paragraph: Paragraph<'static>,
    table: Table<'static>,
    status: Paragraph<'static>,
    layout: Flex,
}

fn build_ascii_scene() -> DashboardScene {
    let mut warmup_text = String::with_capacity(20 * 96);
    let mut measured_text = String::with_capacity(20 * 96);
    for i in 0..20usize {
        warmup_text.push_str(&format!(
            "line {:02} [warmup]: deterministic buffer diff presenter pipeline keeps output minimal and flicker-free across frames\n",
            i + 1
        ));
        measured_text.push_str(&format!(
            "line {:02} [frame1]: deterministic buffer diff presenter pipeline keeps output minimal and flicker-free across frames\n",
            i + 1
        ));
    }

    let warmup_paragraph = Paragraph::new(warmup_text)
        .block(Block::bordered().title("Activity Log"))
        .wrap(WrapMode::Word);
    let measured_paragraph = Paragraph::new(measured_text)
        .block(Block::bordered().title("Activity Log"))
        .wrap(WrapMode::Word);

    let rows: Vec<Row> = (0..10usize)
        .map(|idx| {
            Row::new([
                format!("worker-{idx:02}"),
                if idx % 2 == 0 {
                    "running".to_string()
                } else {
                    "idle".to_string()
                },
                format!("{} us", 40 + idx * 7),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fixed(14),
            Constraint::Fixed(12),
            Constraint::Fill,
        ],
    )
    .header(Row::new(["Worker", "State", "Latency"]))
    .block(Block::bordered().title("Workers"));

    let status = Paragraph::new("STATUS: READY | FPS: 60.0 | BUDGET: OK");

    // 10 constraints (>8 inline SmallVec capacity) so non-arena Flex::split
    // spills to the heap while arena-backed Flex::split_in stays on the bump arena.
    let layout = Flex::vertical().constraints([
        Constraint::Fixed(10),
        Constraint::Fixed(10),
        Constraint::Fixed(1),
        Constraint::Fixed(1),
        Constraint::Fixed(1),
        Constraint::Fixed(1),
        Constraint::Fixed(0),
        Constraint::Fixed(0),
        Constraint::Fixed(0),
        Constraint::Fill,
    ]);

    DashboardScene {
        warmup_paragraph,
        measured_paragraph,
        table,
        status,
        layout,
    }
}

fn build_unicode_scene() -> DashboardScene {
    let mut warmup_text = String::with_capacity(20 * 128);
    let mut measured_text = String::with_capacity(20 * 128);
    for i in 0..20usize {
        warmup_text.push_str(&format!(
            "行 {:02} [準備]: 端末描画パイプラインは差分計算と文字幅キャッシュ 🙂👩‍💻 により高速に動作します\n",
            i + 1
        ));
        measured_text.push_str(&format!(
            "行 {:02} [計測]: 端末描画パイプラインは差分計算と文字幅キャッシュ 🙂👩‍💻 により高速に動作します\n",
            i + 1
        ));
    }

    let warmup_paragraph = Paragraph::new(warmup_text)
        .block(Block::bordered().title("ログ出力"))
        .wrap(WrapMode::Word);
    let measured_paragraph = Paragraph::new(measured_text)
        .block(Block::bordered().title("ログ出力"))
        .wrap(WrapMode::Word);

    let rows: Vec<Row> = (0..10usize)
        .map(|idx| {
            Row::new([
                format!("ワーカー-{idx:02}"),
                if idx % 2 == 0 {
                    "稼働中 🙂".to_string()
                } else {
                    "待機中".to_string()
                },
                format!("{} µs", 45 + idx * 9),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fixed(16),
            Constraint::Fixed(14),
            Constraint::Fill,
        ],
    )
    .header(Row::new(["名前", "状態", "遅延"]))
    .block(Block::bordered().title("ワーカー一覧"));

    let status = Paragraph::new("状態: 正常 | フレームレート: 60.0 | 予算: 範囲内");

    let layout = Flex::vertical().constraints([
        Constraint::Fixed(10),
        Constraint::Fixed(10),
        Constraint::Fixed(1),
        Constraint::Fixed(1),
        Constraint::Fixed(1),
        Constraint::Fixed(1),
        Constraint::Fixed(0),
        Constraint::Fixed(0),
        Constraint::Fixed(0),
        Constraint::Fill,
    ]);

    DashboardScene {
        warmup_paragraph,
        measured_paragraph,
        table,
        status,
        layout,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_dashboard_frame(
    scene: &DashboardScene,
    warmup: bool,
    table_state: &mut TableState,
    pool: &mut GraphemePool,
    arena: Option<&FrameArena>,
    prev_buffer: &Buffer,
    curr_buffer: &mut Option<Buffer>,
    diff: &mut BufferDiff,
    presenter: &mut Presenter<Vec<u8>>,
    is_unicode: bool,
) {
    let mut buf = curr_buffer.take().expect("curr_buffer present");
    buf.clear();
    let mut frame = Frame::from_buffer(buf, pool);
    if let Some(a) = arena {
        frame.set_arena(a);
    }
    let root = Rect::new(0, 0, COLS, ROWS);
    let chunks_owned;
    let chunks: &[Rect] = if let Some(a) = arena {
        scene.layout.split_in(a, root)
    } else {
        chunks_owned = scene.layout.split(root);
        &chunks_owned
    };
    let para = if warmup {
        &scene.warmup_paragraph
    } else {
        &scene.measured_paragraph
    };
    Widget::render(para, chunks[0], &mut frame);
    StatefulWidget::render(&scene.table, chunks[1], &mut frame, table_state);
    Widget::render(&scene.status, chunks[2], &mut frame);

    if is_unicode && !warmup {
        // Explicit per-frame unicode status formatting on the heap to model
        // dynamic multi-codepoint grapheme cluster assembly.
        let badge = format!("🙂 frame-status-{}", chunks.len());
        black_box(badge);
    }

    let rendered = frame.buffer;
    diff.compute_dirty_into(prev_buffer, &rendered);
    presenter.writer_mut().clear();
    presenter
        .present_with_pool(&rendered, diff, Some(pool), None)
        .expect("present dashboard frame");
    *curr_buffer = Some(rendered);
}

fn measure_empty_baseline_frame() -> usize {
    let (_, delta) = measure_allocations(|| {
        black_box(0usize);
    });
    delta.total_alloc_ops()
}

fn measure_scene(scene_name: &str, scene: &DashboardScene, use_arena: bool) -> AllocReportEntry {
    let is_unicode = scene_name.starts_with("unicode");
    let caps = TerminalCapabilities::builder()
        .color_depth(ColorDepth::TrueColor)
        .build();
    let mut pool = GraphemePool::new();
    let mut table_state = TableState::default();
    let mut prev_buffer = Buffer::new(COLS, ROWS);
    let mut curr_buffer = Some(Buffer::new(COLS, ROWS));
    let mut diff = BufferDiff::new();
    let mut presenter = Presenter::new(Vec::with_capacity(16384), caps);
    let mut arena = FrameArena::default();

    let baseline_allocs = measure_empty_baseline_frame();

    // Seed prev_buffer with alternating cells so warm-up pass 1 produces
    // multi-run rows and primes Presenter's RowPlanScratch without rendering
    // `measured_paragraph` into PARAGRAPH_CACHE before the measured frame.
    for y in 0..ROWS {
        for x in (0..COLS).step_by(2) {
            prev_buffer.set_raw(x, y, ftui_render::cell::Cell::from_char('x'));
        }
    }

    // Warm-up pass 1: warms curr_buffer (buffer B) and Presenter scratch.
    render_dashboard_frame(
        scene,
        true,
        &mut table_state,
        &mut pool,
        if use_arena { Some(&arena) } else { None },
        &prev_buffer,
        &mut curr_buffer,
        &mut diff,
        &mut presenter,
        is_unicode,
    );
    std::mem::swap(&mut prev_buffer, curr_buffer.as_mut().unwrap());
    if use_arena {
        arena.reset();
    }

    // Warm-up pass 2: warms the second buffer (buffer A) after swap so both
    // buffers in the double-buffer pair have warmed scissor/dirty-span capacity.
    render_dashboard_frame(
        scene,
        true,
        &mut table_state,
        &mut pool,
        if use_arena { Some(&arena) } else { None },
        &prev_buffer,
        &mut curr_buffer,
        &mut diff,
        &mut presenter,
        is_unicode,
    );
    std::mem::swap(&mut prev_buffer, curr_buffer.as_mut().unwrap());
    if use_arena {
        arena.reset();
    }

    let (_, delta) = measure_allocations(|| {
        render_dashboard_frame(
            scene,
            false,
            &mut table_state,
            &mut pool,
            if use_arena { Some(&arena) } else { None },
            &prev_buffer,
            &mut curr_buffer,
            &mut diff,
            &mut presenter,
            is_unicode,
        );
    });

    let raw_allocs = delta.total_alloc_ops();
    let net_allocs = raw_allocs.saturating_sub(baseline_allocs);
    let arena_bytes = if use_arena {
        arena.allocated_bytes()
    } else {
        0
    };

    let entry = AllocReportEntry {
        scene: scene_name.to_string(),
        arena: use_arena,
        allocs_per_frame: net_allocs,
        raw_allocs_per_frame: raw_allocs,
        baseline_allocs,
        bytes_per_frame: delta.bytes_allocated,
        arena_bytes,
    };
    eprintln!(
        r#"{{"scene":"{}","arena":{},"allocs_per_frame":{},"raw_allocs_per_frame":{},"bytes_per_frame":{},"arena_bytes":{}}}"#,
        entry.scene,
        entry.arena,
        entry.allocs_per_frame,
        entry.raw_allocs_per_frame,
        entry.bytes_per_frame,
        entry.arena_bytes
    );
    entry
}

fn report_output_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FTUI_PERF_REPORT_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("PERF_GATE_RESULTS_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/regression-gate")
}

fn write_alloc_report() {
    assert_counter_counts_box_alloc_and_free();
    assert_counter_is_zero_for_stack_only_work();

    let ascii = build_ascii_scene();
    let unicode = build_unicode_scene();

    let ascii_arena = measure_scene("ascii_dashboard_80x24", &ascii, true);
    let ascii_no_arena = measure_scene("ascii_dashboard_80x24", &ascii, false);
    let unicode_arena = measure_scene("unicode_dashboard_80x24", &unicode, true);
    let unicode_no_arena = measure_scene("unicode_dashboard_80x24", &unicode, false);

    assert_eq!(
        ascii_arena.allocs_per_frame, 0,
        "ascii_dashboard_80x24 with FrameArena must perform 0 heap allocations per frame, got {}",
        ascii_arena.allocs_per_frame
    );
    assert!(
        ascii_no_arena.allocs_per_frame > 0,
        "ascii_dashboard_80x24 without FrameArena must perform >0 heap allocations"
    );
    assert!(
        unicode_arena.allocs_per_frame.saturating_mul(2) <= unicode_no_arena.allocs_per_frame,
        "unicode_dashboard_80x24 with FrameArena ({}) must be <= 50% of non-arena ({})",
        unicode_arena.allocs_per_frame,
        unicode_no_arena.allocs_per_frame
    );

    let entries = vec![ascii_arena, ascii_no_arena, unicode_arena, unicode_no_arena];

    let out_dir = report_output_dir();
    if std::fs::create_dir_all(&out_dir).is_ok() {
        let report_path = out_dir.join("alloc_report.json");
        if let Ok(json) = serde_json::to_string_pretty(&entries) {
            let _ = std::fs::write(report_path, format!("{json}\n"));
        }
    }
}

fn bench_alloc_scenes(c: &mut Criterion) {
    let ascii = build_ascii_scene();
    let unicode = build_unicode_scene();

    let mut group = c.benchmark_group("alloc_count/dashboard_80x24");
    group.bench_function("ascii_with_arena", |b| {
        let caps = TerminalCapabilities::builder()
            .color_depth(ColorDepth::TrueColor)
            .build();
        let mut pool = GraphemePool::new();
        let mut table_state = TableState::default();
        let prev_buffer = Buffer::new(COLS, ROWS);
        let mut curr_buffer = Some(Buffer::new(COLS, ROWS));
        let mut diff = BufferDiff::new();
        let mut presenter = Presenter::new(Vec::with_capacity(16384), caps);
        let mut arena = FrameArena::default();
        b.iter(|| {
            arena.reset();
            render_dashboard_frame(
                &ascii,
                false,
                &mut table_state,
                &mut pool,
                Some(&arena),
                &prev_buffer,
                &mut curr_buffer,
                &mut diff,
                &mut presenter,
                false,
            );
            black_box(presenter.writer_mut().len())
        });
    });

    group.bench_function("unicode_with_arena", |b| {
        let caps = TerminalCapabilities::builder()
            .color_depth(ColorDepth::TrueColor)
            .build();
        let mut pool = GraphemePool::new();
        let mut table_state = TableState::default();
        let prev_buffer = Buffer::new(COLS, ROWS);
        let mut curr_buffer = Some(Buffer::new(COLS, ROWS));
        let mut diff = BufferDiff::new();
        let mut presenter = Presenter::new(Vec::with_capacity(16384), caps);
        let mut arena = FrameArena::default();
        b.iter(|| {
            arena.reset();
            render_dashboard_frame(
                &unicode,
                false,
                &mut table_state,
                &mut pool,
                Some(&arena),
                &prev_buffer,
                &mut curr_buffer,
                &mut diff,
                &mut presenter,
                true,
            );
            black_box(presenter.writer_mut().len())
        });
    });
    group.finish();
}

criterion_group!(benches, bench_alloc_scenes);

fn main() {
    benches();
    write_alloc_report();
}
