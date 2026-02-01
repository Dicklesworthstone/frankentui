//! Benchmarks for Unicode width calculation (bd-16k)
//!
//! Run with: cargo bench -p ftui-text

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use unicode_width::UnicodeWidthStr;

// =============================================================================
// Test Data
// =============================================================================

/// ASCII-only text of various lengths
fn ascii_text(len: usize) -> String {
    "The quick brown fox jumps over the lazy dog. "
        .chars()
        .cycle()
        .take(len)
        .collect()
}

/// CJK text (width 2 per char)
fn cjk_text(len: usize) -> String {
    "\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}\u{6587}\u{672C}"
        .chars()
        .cycle()
        .take(len)
        .collect()
}

/// Mixed ASCII and CJK
fn mixed_text(len: usize) -> String {
    "Hello \u{4E16}\u{754C}! Test \u{6D4B}\u{8BD5}. "
        .chars()
        .cycle()
        .take(len)
        .collect()
}

/// Emoji-heavy text
fn emoji_text(len: usize) -> String {
    "\u{1F600}\u{1F389}\u{1F680}\u{1F4BB}\u{1F3E0}"
        .chars()
        .cycle()
        .take(len)
        .collect()
}

/// Text with combining characters
fn combining_text(len: usize) -> String {
    "e\u{0301}a\u{0300}o\u{0302}u\u{0308}"
        .chars()
        .cycle()
        .take(len)
        .collect()
}

/// ZWJ sequences (complex graphemes)
fn zwj_text(count: usize) -> String {
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".repeat(count)
}

// =============================================================================
// Benchmarks
// =============================================================================

fn bench_ascii_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("width/ascii");

    for len in [10, 100, 1000, 10000] {
        let text = ascii_text(len);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &text, |b, text| {
            b.iter(|| black_box(text.width()))
        });
    }

    group.finish();
}

fn bench_cjk_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("width/cjk");

    for len in [10, 100, 1000, 10000] {
        let text = cjk_text(len);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &text, |b, text| {
            b.iter(|| black_box(text.width()))
        });
    }

    group.finish();
}

fn bench_mixed_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("width/mixed");

    for len in [10, 100, 1000, 10000] {
        let text = mixed_text(len);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &text, |b, text| {
            b.iter(|| black_box(text.width()))
        });
    }

    group.finish();
}

fn bench_emoji_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("width/emoji");

    for len in [10, 100, 1000] {
        let text = emoji_text(len);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &text, |b, text| {
            b.iter(|| black_box(text.width()))
        });
    }

    group.finish();
}

fn bench_combining_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("width/combining");

    for len in [10, 100, 1000] {
        let text = combining_text(len);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &text, |b, text| {
            b.iter(|| black_box(text.width()))
        });
    }

    group.finish();
}

fn bench_zwj_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("width/zwj");

    for count in [1, 10, 50] {
        let text = zwj_text(count);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &text, |b, text| {
            b.iter(|| black_box(text.width()))
        });
    }

    group.finish();
}

fn bench_cache_vs_direct(c: &mut Criterion) {
    use ftui_text::WidthCache;

    let mut group = c.benchmark_group("cache_vs_direct");

    let test_strings: Vec<String> = (0..100).map(|i| format!("string_{}", i)).collect();

    // Direct width calculation
    group.bench_function("direct", |b| {
        b.iter(|| {
            for s in &test_strings {
                black_box(s.width());
            }
        })
    });

    // Cached width calculation (cold cache)
    group.bench_function("cache_cold", |b| {
        b.iter(|| {
            let mut cache = WidthCache::new(1000);
            for s in &test_strings {
                black_box(cache.get_or_compute(s));
            }
        })
    });

    // Cached width calculation (warm cache)
    group.bench_function("cache_warm", |b| {
        let mut cache = WidthCache::new(1000);
        // Warm up cache
        for s in &test_strings {
            cache.get_or_compute(s);
        }
        b.iter(|| {
            for s in &test_strings {
                black_box(cache.get_or_compute(s));
            }
        })
    });

    group.finish();
}

fn bench_segment_width(c: &mut Criterion) {
    use ftui_text::Segment;

    let mut group = c.benchmark_group("segment_width");

    let test_cases = [
        ("ascii", ascii_text(100)),
        ("cjk", cjk_text(100)),
        ("mixed", mixed_text(100)),
        ("emoji", emoji_text(50)),
    ];

    for (name, text) in test_cases {
        let segment = Segment::text(text.as_str());
        group.bench_with_input(BenchmarkId::from_parameter(name), &segment, |b, seg| {
            b.iter(|| black_box(seg.cell_length()))
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_ascii_width,
    bench_cjk_width,
    bench_mixed_width,
    bench_emoji_width,
    bench_combining_width,
    bench_zwj_width,
    bench_cache_vs_direct,
    bench_segment_width,
);

criterion_main!(benches);
