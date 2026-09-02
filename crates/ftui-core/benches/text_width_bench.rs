//! Grapheme width: cached (production `grapheme_width`) versus uncached.
//!
//! Run with: cargo bench -p ftui-core --bench text_width_bench
//!
//! The corpus mirrors what a busy screen re-measures every frame: a table of
//! CJK labels, emoji status glyphs (including ZWJ sequences and flags), Latin
//! with combining marks, and plain ASCII filler. `cached` is the production
//! path (`text_width::grapheme_width`, S3-FIFO in front of the Unicode
//! tables); `uncached` calls `grapheme_width_uncached` directly.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::text_width::{clear_width_cache, grapheme_width, grapheme_width_uncached};
use std::hint::black_box;
use unicode_segmentation::UnicodeSegmentation;

fn corpus() -> Vec<String> {
    let rows = [
        "名前 │ 状態 │ 更新 │ 備考",
        "サービス │ 稼働中 ✅ │ 3分前 │ 正常",
        "데이터베이스 │ 경고 ⚠️ │ 12분 전 │ 지연",
        "worker-07 │ down ❌ │ 1h ago │ 👨‍👩‍👧‍👦 on call",
        "🇯🇵 tokyo │ ok │ 5s │ résumé café naïve",
        "🏳️‍🌈 pride │ ok │ 9s │ ẹ̃ x̣ combining stack",
        "plain ascii row with nothing special at all 0123456789",
    ];
    rows.iter()
        .cycle()
        .take(rows.len() * 8)
        .map(|row| (*row).to_string())
        .collect()
}

fn graphemes(rows: &[String]) -> Vec<&str> {
    rows.iter()
        .flat_map(|row| row.graphemes(true))
        .filter(|g| !g.is_ascii())
        .collect()
}

fn bench_width(c: &mut Criterion) {
    let rows = corpus();
    let clusters = graphemes(&rows);
    let mut group = c.benchmark_group("text_width/non_ascii_grapheme");
    group.throughput(Throughput::Elements(clusters.len() as u64));

    group.bench_with_input(
        BenchmarkId::new("uncached", clusters.len()),
        &clusters,
        |b, cl| {
            b.iter(|| {
                let mut total = 0usize;
                for g in cl {
                    total += grapheme_width_uncached(black_box(g));
                }
                black_box(total)
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("cached", clusters.len()),
        &clusters,
        |b, cl| {
            // Warm once so the measurement is the steady state a running screen sees.
            clear_width_cache();
            for g in cl {
                let _ = grapheme_width(g);
            }
            b.iter(|| {
                let mut total = 0usize;
                for g in cl {
                    total += grapheme_width(black_box(g));
                }
                black_box(total)
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("cached_cold", clusters.len()),
        &clusters,
        |b, cl| {
            b.iter(|| {
                clear_width_cache();
                let mut total = 0usize;
                for g in cl {
                    total += grapheme_width(black_box(g));
                }
                black_box(total)
            });
        },
    );
    group.finish();
}

criterion_group!(benches, bench_width);
criterion_main!(benches);
