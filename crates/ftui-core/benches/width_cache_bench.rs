//! Benchmark: Cached vs uncached non-ASCII grapheme width lookup.
//!
//! Run with: cargo bench -p ftui-core --bench width_cache_bench
//!
//! Measures steady-state per-grapheme lookup performance over a 200-grapheme
//! non-ASCII corpus sampled Zipf(1.0) with a fixed LCG seed.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::text_width::{clear_width_cache, grapheme_width, grapheme_width_uncached};
use std::hint::black_box;

fn build_distinct_graphemes() -> Vec<String> {
    let mut graphemes = Vec::with_capacity(200);

    // 100 CJK characters (common Chinese/Japanese/Korean glyphs)
    for i in 0..100 {
        let cp = 0x4e00 + i;
        graphemes.push(char::from_u32(cp).unwrap().to_string());
    }

    // 40 Emojis (single, skin tone, ZWJ sequences, flags)
    let emojis = [
        "😀",
        "😃",
        "😄",
        "😁",
        "😆",
        "😅",
        "😂",
        "🤣",
        "😊",
        "😇",
        "🙂",
        "🙃",
        "😉",
        "😌",
        "😍",
        "🥰",
        "😘",
        "😗",
        "😙",
        "😚",
        "👋🏽",
        "👍🏻",
        "👎🏿",
        "🏃🏿‍♂️",
        "👩🏻‍💻",
        "👨‍👩‍👧‍👦",
        "🇯🇵",
        "🇺🇸",
        "🏳️‍🌈",
        "❤️",
        "⚠️",
        "✅",
        "❌",
        "🔥",
        "✨",
        "🎉",
        "🚀",
        "💡",
        "📦",
        "⚡",
    ];
    for &emoji in &emojis {
        graphemes.push(emoji.to_string());
    }

    // 40 Combining mark sequences (Latin base + combining accents)
    for i in 0..40 {
        let mark = char::from_u32(0x0300 + i).unwrap();
        graphemes.push(format!("e{mark}"));
    }

    // 20 Other non-ASCII scripts (Cyrillic, Greek, Thai, Devanagari)
    let others = [
        "Ж", "Д", "Л", "Ф", "Ц", "Ч", "Ш", "Щ", "Ю", "Я", // Cyrillic
        "Ω", "Ψ", "Δ", "Σ", "Θ", // Greek
        "ก", "ข", "ค", // Thai
        "क", "ख", // Devanagari
    ];
    for &ch in &others {
        graphemes.push(ch.to_string());
    }

    assert_eq!(graphemes.len(), 200);
    graphemes
}

/// Simple 64-bit Linear Congruential Generator (LCG).
struct SimpleLcg {
    state: u64,
}

impl SimpleLcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // Knuth's 64-bit LCG constants
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    fn next_f64(&mut self) -> f64 {
        // Upper 53 bits normalized to [0.0, 1.0)
        let bits = self.next_u64() >> 11;
        (bits as f64) / ((1u64 << 53) as f64)
    }
}

/// Sample `sample_count` items from the 200-item corpus using a Zipf(1.0) distribution.
fn sample_zipf(items: &[String], sample_count: usize, seed: u64) -> Vec<String> {
    let n = items.len();
    assert_eq!(n, 200);

    // Compute Zipf(1.0) cumulative distribution
    // Weight for rank k (1-indexed): 1.0 / k
    let mut cumulative = Vec::with_capacity(n);
    let mut sum = 0.0;
    for k in 1..=n {
        sum += 1.0 / (k as f64);
        cumulative.push(sum);
    }

    let mut lcg = SimpleLcg::new(seed);
    let mut samples = Vec::with_capacity(sample_count);

    for _ in 0..sample_count {
        let r = lcg.next_f64() * sum;
        let idx = match cumulative.binary_search_by(|v| v.partial_cmp(&r).unwrap()) {
            Ok(i) => i,
            Err(i) => i.min(n - 1),
        };
        samples.push(items[idx].clone());
    }

    samples
}

fn bench_width_cache(c: &mut Criterion) {
    let distinct = build_distinct_graphemes();
    let sample_corpus = sample_zipf(&distinct, 10_000, 0x1234_5678_9ABC_DEF0);

    let mut group = c.benchmark_group("text_width/non_ascii_corpus");
    group.throughput(Throughput::Elements(1));

    // Warm up the cache with 10,000 lookups
    clear_width_cache();
    for g in &sample_corpus {
        let _ = grapheme_width(g);
    }

    let mut cached_idx = 0usize;
    group.bench_function("cached", |b| {
        b.iter(|| {
            let g = &sample_corpus[cached_idx % sample_corpus.len()];
            cached_idx = cached_idx.wrapping_add(1);
            black_box(grapheme_width(black_box(g)))
        });
    });

    let mut uncached_idx = 0usize;
    group.bench_function("uncached", |b| {
        b.iter(|| {
            let g = &sample_corpus[uncached_idx % sample_corpus.len()];
            uncached_idx = uncached_idx.wrapping_add(1);
            black_box(grapheme_width_uncached(black_box(g)))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_width_cache);
criterion_main!(benches);
