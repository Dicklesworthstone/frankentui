//! Benchmarks for virtualized list scroll positioning and height-prediction scroll jump (G10).
//!
//! Run with:
//! `cargo bench -p ftui-widgets --bench virtualized_bench`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_widgets::height_predictor::PredictorConfig;
use ftui_widgets::virtualized::{
    HeightCache, ItemHeight, RenderItem, VariableHeightsFenwick, Virtualized,
};
use serde::Serialize;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
struct CorpusRow {
    height: u16,
    category: usize,
}

impl RenderItem for CorpusRow {
    fn render(&self, _area: Rect, _frame: &mut Frame, _selected: bool, _skip_rows: u16) {}

    fn height(&self) -> u16 {
        self.height
    }

    fn category(&self) -> usize {
        self.category
    }
}

fn generate_long_tail_corpus(n: usize, seed: u64) -> (Vec<CorpusRow>, Vec<usize>) {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut next_u64 = || -> u64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    };

    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let bucket = (next_u64() >> 32) % 100;
        let (height, category) = if bucket < 80 {
            (1u16, 0usize)
        } else if bucket < 95 {
            let r = next_u64();
            let h = 2 + (((r >> 32) % 2) + ((r >> 48) % 2)) as u16;
            (h, 1usize)
        } else {
            let r = next_u64();
            let tier = ((r >> 48) % 3) as usize;
            let jitter = (((r & 0xFF) % 3) + (((r >> 8) & 0xFF) % 3)) as u16;
            let h = match tier {
                0 => 6 + jitter,
                1 => 15 + jitter,
                _ => 25 + jitter,
            };
            (h, 2 + tier)
        };
        rows.push(CorpusRow { height, category });
    }

    let mut perm: Vec<usize> = (0..n).collect();
    for idx in (1..n).rev() {
        let swap_idx = (next_u64() as usize) % (idx + 1);
        perm.swap(idx, swap_idx);
    }

    (rows, perm)
}

fn bench_virtualized_scroll_to(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/scroll_to");

    for &n in &[10_000usize, 100_000usize] {
        let (rows, _) = generate_long_tail_corpus(n, 0xCAFE_BABE_2026_0910);
        let heights: Vec<u16> = rows.iter().map(|r| r.height).collect();
        let target = n / 2;

        let mut fixed_list: Virtualized<CorpusRow> =
            Virtualized::external(n, 0).with_fixed_height(2);
        group.bench_with_input(BenchmarkId::new("fixed", n), &target, |b, &idx| {
            b.iter(|| {
                fixed_list.scroll_to(black_box(idx));
                black_box(fixed_list.offset_of_item(black_box(idx)))
            });
        });

        let mut cache = HeightCache::new(2, n);
        for (i, &h) in heights.iter().enumerate() {
            cache.set(i, h);
        }
        let mut linear_list: Virtualized<CorpusRow> =
            Virtualized::external(n, 0).with_item_height(ItemHeight::Variable(cache));
        group.bench_with_input(
            BenchmarkId::new("variable_linear", n),
            &target,
            |b, &idx| {
                b.iter(|| {
                    linear_list.scroll_to(black_box(idx));
                    black_box(linear_list.offset_of_item(black_box(idx)))
                });
            },
        );

        let fenwick = VariableHeightsFenwick::from_heights(&heights, 2);
        let mut fenwick_list: Virtualized<CorpusRow> =
            Virtualized::external(n, 0).with_item_height(ItemHeight::VariableFenwick(fenwick));
        group.bench_with_input(
            BenchmarkId::new("variable_fenwick", n),
            &target,
            |b, &idx| {
                b.iter(|| {
                    fenwick_list.scroll_to(black_box(idx));
                    black_box(fenwick_list.offset_of_item(black_box(idx)))
                });
            },
        );
    }

    group.finish();
}

#[derive(Debug, Clone, Serialize)]
struct ScrollJumpEntry {
    variant: &'static str,
    rows: usize,
    jump_cells: u64,
    measurements: usize,
    generated_at: &'static str,
    git_commit: String,
}

fn run_scroll_jump_variant(
    variant: &'static str,
    rows: &[CorpusRow],
    perm: &[usize],
    mean_height: u16,
) -> (u64, usize) {
    let n = rows.len();
    let target = n / 2;
    let train_count = (n / 10).max(1);

    let mut list: Virtualized<CorpusRow> = match variant {
        "fixed_1" => Virtualized::new(n).with_variable_heights(1),
        "mean_baseline" => Virtualized::new(n).with_variable_heights(mean_height),
        "predictor" => Virtualized::new(n)
            .with_variable_heights(1)
            .with_height_predictor(PredictorConfig::default()),
        _ => unreachable!(),
    };

    for &row in rows {
        list.push(row);
    }

    let use_predictor = variant == "predictor";
    for &idx in &perm[..train_count] {
        let h = rows[idx].height;
        if use_predictor {
            list.measure(idx, h);
        } else {
            list.observe_height(idx, h);
        }
    }

    list.scroll_to(target);
    let mut prev_offset = list.offset_of_item(target);
    let mut jump_cells = 0u64;

    for chunk in perm[train_count..].chunks(50) {
        for &idx in chunk {
            let h = rows[idx].height;
            if use_predictor {
                list.measure(idx, h);
            } else {
                list.observe_height(idx, h);
            }
        }
        let next_offset = list.offset_of_item(target);
        jump_cells += u64::from(next_offset.abs_diff(prev_offset));
        prev_offset = next_offset;
    }

    (jump_cells, n)
}

fn bench_virtualized_scroll_jump(c: &mut Criterion) {
    let n = 100_000usize;
    let (rows, perm) = generate_long_tail_corpus(n, 0xD1B5_4A32_D192_ED03);
    let total_sum: u64 = rows.iter().map(|r| u64::from(r.height)).sum();
    let mean_height = ((total_sum as f64) / (n as f64)).round().max(1.0) as u16;
    let git_commit = std::env::var("RCH_BUILD_SOURCE").unwrap_or_else(|_| "HEAD".to_string());

    let mut entries = Vec::new();
    let mut group = c.benchmark_group("virtualized/scroll_jump");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(1));

    for &variant in &["fixed_1", "mean_baseline", "predictor"] {
        let (jump_cells, measurements) =
            run_scroll_jump_variant(variant, &rows, &perm, mean_height);
        eprintln!(
            "[virtualized_scroll_jump] variant={variant} rows={n} jump_cells={jump_cells} measurements={measurements}"
        );
        entries.push(ScrollJumpEntry {
            variant,
            rows: n,
            jump_cells,
            measurements,
            generated_at: "2026-10-16T00:00:00Z",
            git_commit: git_commit.clone(),
        });

        group.bench_with_input(BenchmarkId::new(variant, n), &n, |b, _| {
            b.iter_custom(|iters| {
                let start = Instant::now();
                for _ in 0..iters {
                    let (jc, m) = run_scroll_jump_variant(variant, &rows, &perm, mean_height);
                    black_box((jc, m));
                }
                start.elapsed()
            });
        });
    }

    group.finish();

    if let Ok(json) = serde_json::to_string_pretty(&entries) {
        let mut paths = vec![PathBuf::from(
            "target/criterion/virtualized_scroll_jump.json",
        )];
        if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
            paths.push(PathBuf::from(target_dir).join("criterion/virtualized_scroll_jump.json"));
        }
        for path in paths {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&path, &json);
        }
    }
}

criterion_group!(
    benches,
    bench_virtualized_scroll_to,
    bench_virtualized_scroll_jump
);
criterion_main!(benches);
