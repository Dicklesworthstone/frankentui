//! Portable-SIMD kernels against their scalar twins (bd-g00-root-epic-ewths.38.2)
//!
//! Run with: cargo bench -p ftui-simd --bench simd_bench
//!
//! Every group pairs `scalar/` and `simd/` over the same input so the ratio is
//! read straight off two lines of the same report. Sizes are the ones the
//! render path actually sees: 80 and 200 cells are terminal row widths, 1000
//! is a wide row or a joined run; 64 bytes is one vector chunk, 1024 a long
//! line, 65536 a pathological paste.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_simd::{
    all_ascii, all_ascii_scalar, ascii_width, ascii_width_scalar, first_mismatch_u128,
    first_mismatch_u128_scalar,
};
use std::hint::black_box;

/// Deterministic cell content, so two runs of the suite compare like with like.
fn cells(len: usize) -> Vec<u128> {
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            u128::from(state) | (u128::from(state.rotate_left(29)) << 64)
        })
        .collect()
}

/// Rows that differ only in their last cell.
///
/// This is the worst case and the one worth measuring: the kernel cannot exit
/// early, so the whole row goes through the lanes. A row that differs at cell
/// zero says nothing about throughput.
fn diverging_at_end(len: usize) -> (Vec<u128>, Vec<u128>) {
    let base = cells(len);
    let mut changed = base.clone();
    if let Some(last) = changed.last_mut() {
        *last ^= 1;
    }
    (base, changed)
}

fn bench_first_mismatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd/first_mismatch");
    for len in [80_usize, 200, 1000] {
        let (a, b) = diverging_at_end(len);
        group.throughput(Throughput::Elements(len as u64));

        group.bench_with_input(
            BenchmarkId::new("scalar", format!("{len}_cells")),
            &(&a, &b),
            |bencher, (a, b)| {
                bencher.iter(|| black_box(first_mismatch_u128_scalar(black_box(a), black_box(b))));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("simd", format!("{len}_cells")),
            &(&a, &b),
            |bencher, (a, b)| {
                bencher.iter(|| black_box(first_mismatch_u128(black_box(a), black_box(b))));
            },
        );
    }
    group.finish();
}

/// Identical rows, which is what the diff sees on most frames.
fn bench_rows_equal(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd/rows_equal");
    for len in [80_usize, 200, 1000] {
        let a = cells(len);
        let b = a.clone();
        group.throughput(Throughput::Elements(len as u64));

        group.bench_with_input(
            BenchmarkId::new("scalar", format!("{len}_cells")),
            &(&a, &b),
            |bencher, (a, b)| {
                bencher.iter(|| black_box(first_mismatch_u128_scalar(black_box(a), black_box(b))));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("simd", format!("{len}_cells")),
            &(&a, &b),
            |bencher, (a, b)| {
                bencher.iter(|| black_box(first_mismatch_u128(black_box(a), black_box(b))));
            },
        );
    }
    group.finish();
}

fn bench_all_ascii(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd/all_ascii");
    for len in [64_usize, 1024, 65536] {
        // All-ASCII is the case that has to scan everything; a non-ASCII byte
        // only makes both twins return sooner.
        let bytes = vec![b'a'; len];
        group.throughput(Throughput::Bytes(len as u64));

        group.bench_with_input(
            BenchmarkId::new("scalar", format!("{len}_bytes")),
            &bytes,
            |bencher, bytes| {
                bencher.iter(|| black_box(all_ascii_scalar(black_box(bytes))));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("simd", format!("{len}_bytes")),
            &bytes,
            |bencher, bytes| {
                bencher.iter(|| black_box(all_ascii(black_box(bytes))));
            },
        );
    }
    group.finish();
}

fn bench_ascii_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd/ascii_width");
    for len in [64_usize, 1024, 65536] {
        let bytes = vec![b'a'; len];
        group.throughput(Throughput::Bytes(len as u64));

        group.bench_with_input(
            BenchmarkId::new("scalar", format!("{len}_bytes")),
            &bytes,
            |bencher, bytes| {
                bencher.iter(|| black_box(ascii_width_scalar(black_box(bytes))));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("simd", format!("{len}_bytes")),
            &bytes,
            |bencher, bytes| {
                bencher.iter(|| black_box(ascii_width(black_box(bytes))));
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_first_mismatch,
    bench_rows_equal,
    bench_all_ascii,
    bench_ascii_width
);
criterion_main!(benches);
