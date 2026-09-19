//! Line-breaking throughput for the wrap modes the render path uses
//! (bd-g00-root-epic-ewths.31.2).
//!
//! Run with: cargo bench -p ftui-text --bench wrap_bench
//!
//! Wrapping is on the hot path for every Paragraph and log line, and the
//! interesting question is not "how fast is one call" but how the greedy word
//! wrap compares with Knuth-Plass over the same corpus, and how much Unicode
//! costs relative to ASCII. The three groups here answer exactly those, over
//! one fixed corpus so the numbers stay comparable between runs.
//!
//! The corpora are generated from fixed formulas rather than random text: a
//! bench whose input changes between runs cannot detect a regression.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ftui_text::wrap::{WrapMode, wrap_text, wrap_text_optimal};
use std::hint::black_box;

/// Words used to build the ASCII corpus, varied in length so line breaks fall
/// in different places rather than at a fixed stride.
const WORDS: &[&str] = &[
    "terminal",
    "buffer",
    "a",
    "deterministic",
    "diff",
    "renders",
    "the",
    "minimal",
    "set",
    "of",
    "cells",
    "that",
    "actually",
    "changed",
    "between",
    "two",
    "frames",
    "without",
    "flicker",
];

/// CJK and emoji words for the Unicode corpus. These are the cases that cost
/// real time: every cluster misses the ASCII fast path and goes through the
/// width tables.
const WIDE_WORDS: &[&str] = &[
    "端末",
    "描画",
    "文字幅",
    "日本語のテキスト",
    "🙂",
    "👩‍💻",
    "테스트",
    "中文字符",
];

/// 200 lines of roughly 60-120 characters, assembled deterministically.
fn ascii_corpus() -> String {
    let mut out = String::with_capacity(200 * 100);
    for line in 0..200_usize {
        let target = 60 + (line * 7) % 61; // 60..=120
        let mut len = 0;
        let mut word = line;
        while len < target {
            let w = WORDS[word % WORDS.len()];
            out.push_str(w);
            out.push(' ');
            len += w.len() + 1;
            word += 1;
        }
        out.push('\n');
    }
    out
}

/// The same shape with every third line made of CJK and emoji, which is the
/// mix a log viewer or a chat pane actually sees.
fn unicode_corpus() -> String {
    let mut out = String::with_capacity(200 * 120);
    for line in 0..200_usize {
        let wide = line % 3 == 0;
        let target = 60 + (line * 7) % 61;
        let mut len = 0;
        let mut word = line;
        while len < target {
            let w = if wide {
                WIDE_WORDS[word % WIDE_WORDS.len()]
            } else {
                WORDS[word % WORDS.len()]
            };
            out.push_str(w);
            out.push(' ');
            // Count display columns, not bytes, so both corpora wrap to a
            // comparable number of lines.
            len += ftui_text::display_width(w) + 1;
            word += 1;
        }
        out.push('\n');
    }
    out
}

fn bench_wrap(c: &mut Criterion) {
    let ascii = ascii_corpus();
    let unicode = unicode_corpus();

    let mut group = c.benchmark_group("wrap");
    // Throughput in lines makes the three numbers directly comparable even
    // though the Unicode corpus is far larger in bytes.
    group.throughput(Throughput::Elements(200));

    group.bench_function("word/200_lines_80_cols", |b| {
        b.iter(|| black_box(wrap_text(black_box(&ascii), 80, WrapMode::Word)));
    });

    group.bench_function("word/200_lines_80_cols_unicode", |b| {
        b.iter(|| black_box(wrap_text(black_box(&unicode), 80, WrapMode::Word)));
    });

    group.bench_function("optimal/200_lines_80_cols", |b| {
        b.iter(|| black_box(wrap_text_optimal(black_box(&ascii), 80)));
    });

    group.finish();
}

criterion_group!(benches, bench_wrap);
criterion_main!(benches);
