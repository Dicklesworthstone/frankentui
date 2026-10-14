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

    // More distinct clusters than the production cache can retain. Include
    // insertion, key allocation and eviction in the cached timing; this is
    // the countermetric to the repeated-screen steady-state measurement.
    let scan: Vec<String> = (0..5_000)
        .map(|index| {
            let base = char::from_u32(0x4e00 + index).expect("CJK scalar");
            format!("{base}\u{0301}")
        })
        .collect();
    let mut group = c.benchmark_group("text_width/scan_pressure");
    group.throughput(Throughput::Elements(scan.len() as u64));
    for (name, measure) in [
        ("uncached", grapheme_width_uncached as fn(&str) -> usize),
        ("cached", grapheme_width as fn(&str) -> usize),
    ] {
        group.bench_with_input(BenchmarkId::new(name, scan.len()), &scan, |b, cl| {
            clear_width_cache();
            b.iter(|| {
                let total: usize = cl.iter().map(|g| measure(black_box(g))).sum();
                black_box(total)
            });
        });
    }
    group.finish();

    // Paired same-invocation comparison of `display_width` strategies across
    // realistic corpora (bd-g00-root-epic-ewths.31.11):
    // 1. uncached_direct: sum of `grapheme_width_uncached` (no S3-FIFO lookup)
    // 2. cached_per_cluster: production `display_width` (ASCII inline, non-ASCII via S3-FIFO)
    // 3. hybrid_multibyte_only: S3-FIFO only for >=3-byte clusters, uncached for 2-byte Latin-1/Arabic
    let display_corpora: [(&str, &[&str]); 4] = [
        (
            "cjk_table",
            &[
                "名前 │ 状態 │ 更新 │ 備考 │ サービス稼働状況",
                "サービス │ 稼働中 ✅ │ 3分前 │ 正常 │ 東京リージョン",
                "데이터베이스 │ 경고 ⚠️ │ 12분 전 │ 지연 │ 서울 리전",
                "キャッシュ層 │ 正常 │ 1秒前 │ ヒット率 98.4%",
            ],
        ),
        (
            "emoji_logs",
            &[
                "2026-10-14T03:00:00Z INFO worker-07 ✅ deploy complete 👋🏽 🚀 ✨",
                "2026-10-14T03:00:01Z WARN alert ⚠️ on-call 👨‍👩‍👧‍👦 👩🏻‍💻 🏃🏿‍♂️ Investigating 🇯🇵 🇺🇸",
                "2026-10-14T03:00:02Z INFO feature 🏳️‍🌈 rollout 🔥 100% ❤️ ☂\u{FE0F} 📦 ⚡",
                "2026-10-14T03:00:03Z ERROR shard-03 ❌ connection reset 🎉 💡",
            ],
        ),
        (
            "mixed_latin1",
            &[
                "résumé café naïve façade coopération über straße",
                "señorita piñata mañana jalapeño corazón",
                "Ångström Øresund Ærøskøbing Smörgåsbord",
                "combining a\u{0301}e\u{0301}i\u{0302}o\u{0303}u\u{0308} accents",
            ],
        ),
        (
            "arabic",
            &[
                "مرحبا بالعالم — حالة النظام: مستقر ✅",
                "قاعدة البيانات │ استجابة سريعة │ ١٢ ملي ثانية",
                "تحديث الواجهة النصية بدون وميض أو تشويه",
                "سجل الأحداث المباشر: لا توجد أخطاء في الخادم",
            ],
        ),
    ];

    let display_uncached = |line: &str| -> usize {
        if let Some(w) = ftui_core::text_width::ascii_width(line) {
            return w;
        }
        line.graphemes(true).map(grapheme_width_uncached).sum()
    };
    let display_hybrid = |line: &str| -> usize {
        if let Some(w) = ftui_core::text_width::ascii_width(line) {
            return w;
        }
        line.graphemes(true)
            .map(|g| {
                if g.len() >= 3 {
                    grapheme_width(g)
                } else {
                    grapheme_width_uncached(g)
                }
            })
            .sum()
    };

    let mut dw_group = c.benchmark_group("text_width/display_width");
    for (corpus_name, sample_lines) in display_corpora {
        let lines: Vec<String> = sample_lines
            .iter()
            .cycle()
            .take(sample_lines.len() * 8)
            .map(|s| (*s).to_string())
            .collect();
        dw_group.throughput(Throughput::Elements(lines.len() as u64));

        dw_group.bench_with_input(
            BenchmarkId::new("uncached_direct", corpus_name),
            &lines,
            |b, rows| {
                b.iter(|| {
                    let total: usize = rows.iter().map(|r| display_uncached(black_box(r))).sum();
                    black_box(total)
                });
            },
        );

        dw_group.bench_with_input(
            BenchmarkId::new("cached_per_cluster", corpus_name),
            &lines,
            |b, rows| {
                clear_width_cache();
                for r in rows {
                    let _ = ftui_core::text_width::display_width(r);
                }
                b.iter(|| {
                    let total: usize = rows
                        .iter()
                        .map(|r| ftui_core::text_width::display_width(black_box(r)))
                        .sum();
                    black_box(total)
                });
            },
        );

        dw_group.bench_with_input(
            BenchmarkId::new("hybrid_multibyte_only", corpus_name),
            &lines,
            |b, rows| {
                clear_width_cache();
                for r in rows {
                    let _ = display_hybrid(r);
                }
                b.iter(|| {
                    let total: usize = rows.iter().map(|r| display_hybrid(black_box(r))).sum();
                    black_box(total)
                });
            },
        );
    }
    dw_group.finish();
}

criterion_group!(benches, bench_width);
criterion_main!(benches);
