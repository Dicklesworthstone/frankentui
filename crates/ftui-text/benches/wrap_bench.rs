#![forbid(unsafe_code)]

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

use criterion::{Criterion, Throughput, criterion_group};
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

    let mut word_group = c.benchmark_group("wrap/word");
    // Throughput in lines makes the three numbers directly comparable even
    // though the Unicode corpus is far larger in bytes.
    word_group.throughput(Throughput::Elements(200));

    word_group.bench_function("200_lines_80_cols", |b| {
        b.iter(|| black_box(wrap_text(black_box(&ascii), 80, WrapMode::Word)));
    });

    word_group.bench_function("200_lines_80_cols_unicode", |b| {
        b.iter(|| black_box(wrap_text(black_box(&unicode), 80, WrapMode::Word)));
    });
    word_group.finish();

    let mut optimal_group = c.benchmark_group("wrap/optimal");
    optimal_group.throughput(Throughput::Elements(200));
    optimal_group.bench_function("200_lines_80_cols", |b| {
        b.iter(|| black_box(wrap_text_optimal(black_box(&ascii), 80)));
    });
    optimal_group.finish();
}

fn emit_criterion_jsonl(bench_name: &str, ids: &[&str]) {
    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());
    for &id in ids {
        let estimates_path = std::path::Path::new(&target_dir)
            .join("criterion")
            .join(id)
            .join("new")
            .join("estimates.json");
        if let Ok(text) = std::fs::read_to_string(&estimates_path)
            && let Some(mean_ns) = extract_point_estimate(&text, "mean")
        {
            let p50_ns = extract_point_estimate(&text, "median").unwrap_or(mean_ns);
            let p99_ns = extract_upper_ci(&text, "mean").unwrap_or(mean_ns);
            eprintln!(
                r#"{{"bench":"{bench_name}","id":"{id}","mean_ns":{mean_ns:.2},"p50_ns":{p50_ns:.2},"p99_ns":{p99_ns:.2}}}"#
            );
        }
    }
}

fn extract_point_estimate(json: &str, section: &str) -> Option<f64> {
    let key = format!("\"{section}\":");
    let start = json.find(&key)? + key.len();
    let rest = &json[start..];
    let pe_key = "\"point_estimate\":";
    let pe_start = rest.find(pe_key)? + pe_key.len();
    let tail = rest[pe_start..].trim_start();
    let end = tail.find([',', '}', ' ', '\n', '\r'])?;
    tail[..end].parse::<f64>().ok()
}

fn extract_upper_ci(json: &str, section: &str) -> Option<f64> {
    let key = format!("\"{section}\":");
    let start = json.find(&key)? + key.len();
    let rest = &json[start..];
    let ci_key = "\"upper_bound\":";
    let ci_start = rest.find(ci_key)? + ci_key.len();
    let tail = rest[ci_start..].trim_start();
    let end = tail.find([',', '}', ' ', '\n', '\r'])?;
    tail[..end].parse::<f64>().ok()
}

criterion_group!(benches, bench_wrap);

fn main() {
    benches();
    emit_criterion_jsonl(
        "wrap_bench",
        &[
            "wrap/word/200_lines_80_cols",
            "wrap/word/200_lines_80_cols_unicode",
            "wrap/optimal/200_lines_80_cols",
        ],
    );
}
