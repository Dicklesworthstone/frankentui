use std::hint::black_box;
use std::process::Command;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use frankenterm_core::{
    Action, Cell, Color, DirtyTracker, Grid, GridDiff, Parser, Patch, SgrAttrs,
};

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn git_sha() -> Option<String> {
    if let Ok(sha) = std::env::var("GITHUB_SHA")
        && !sha.trim().is_empty()
    {
        return Some(sha);
    }

    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

struct Corpus<'a> {
    id: &'a str,
    bytes: &'a [u8],
}

fn corpora() -> Vec<Corpus<'static>> {
    // Keep corpora stable and explicitly versioned by their id+hash.
    const BUILD_LOG: &[u8] = br#"Compiling frankenterm-core v0.1.0 (/repo/crates/frankenterm-core)
Compiling ftui-core v0.1.1 (/repo/crates/ftui-core)
Finished dev [unoptimized + debuginfo] target(s) in 0.73s
"#;

    const DENSE_SGR: &[u8] = b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[33mYELLOW\x1b[0m\n\
\x1b[38;5;196mIDX196\x1b[0m \x1b[38;2;1;2;3mRGB\x1b[0m\n";

    const MARKDOWNISH: &[u8] = br#"# Title
- item one
- item two

```rust
println!("hello");
```
"#;

    // NOTE: The core parser currently ignores non-ASCII bytes in the skeleton.
    // We still include a unicode-heavy stream so throughput numbers remain
    // representative for "real" output streams even before full UTF-8 support.
    const UNICODE_HEAVY: &[u8] = "unicode: café — 你好 — 😀\nline2: e\u{301}\n".as_bytes();

    vec![
        Corpus {
            id: "build_log_v1",
            bytes: BUILD_LOG,
        },
        Corpus {
            id: "dense_sgr_v1",
            bytes: DENSE_SGR,
        },
        Corpus {
            id: "markdownish_v1",
            bytes: MARKDOWNISH,
        },
        Corpus {
            id: "unicode_heavy_v1",
            bytes: UNICODE_HEAVY,
        },
    ]
}

fn parser_throughput_bench(c: &mut Criterion) {
    let sha = git_sha();
    eprintln!(
        "[frankenterm-core bench] git_sha={}",
        sha.as_deref().unwrap_or("<unknown>")
    );

    let mut group = c.benchmark_group("parser_throughput");
    for corpus in corpora() {
        let hash = fnv1a64(corpus.bytes);
        eprintln!(
            "[frankenterm-core bench] corpus={} bytes={} fnv1a64={:016x}",
            corpus.id,
            corpus.bytes.len(),
            hash
        );

        group.throughput(Throughput::Bytes(corpus.bytes.len() as u64));

        // Baseline: allocate the Vec<Action> for each chunk (Parser::feed).
        group.bench_with_input(
            BenchmarkId::new("feed_vec", corpus.id),
            &corpus.bytes,
            |b, bytes| {
                let mut parser = Parser::new();
                b.iter(|| {
                    let actions = parser.feed(black_box(bytes));
                    black_box(actions.len());
                });
            },
        );

        // Lower-bound parse cost: avoid allocating a Vec<Action> by using advance().
        group.bench_with_input(
            BenchmarkId::new("advance_count", corpus.id),
            &corpus.bytes,
            |b, bytes| {
                let mut parser = Parser::new();
                b.iter(|| {
                    let mut count = 0u64;
                    for &b in black_box(*bytes) {
                        if parser.advance(b).is_some() {
                            count += 1;
                        }
                    }
                    black_box(count);
                });
            },
        );
    }
    group.finish();
}

fn apply_patch(grid: &mut Grid, patch: &Patch) {
    for update in &patch.updates {
        if let Some(cell) = grid.cell_mut(update.row, update.col) {
            *cell = update.cell;
        }
    }
}

fn make_old_new_grid(cols: u16, rows: u16, change_count: usize) -> (Grid, Grid) {
    let mut old = Grid::new(cols, rows);
    let mut new = old.clone();

    for i in 0..change_count {
        let row = (i as u16) % rows;
        let col = ((i as u16) * 7) % cols;
        if let Some(cell) = new.cell_mut(row, col) {
            let ch = (b'A' + (i as u8 % 26)) as char;
            cell.set_content(ch, 1);
            cell.attrs = SgrAttrs {
                fg: Color::Named((i as u8) % 16),
                bg: Color::Default,
                ..SgrAttrs::default()
            };
        }
    }

    // Touch one cell in old so the compiler can't trivially treat it as a constant.
    if let Some(cell) = old.cell_mut(0, 0) {
        *cell = Cell::default();
    }

    (old, new)
}

fn make_dirty_tracker(cols: u16, rows: u16, change_count: usize) -> DirtyTracker {
    let mut tracker = DirtyTracker::new(cols, rows);
    for i in 0..change_count {
        let row = (i as u16) % rows;
        let col = ((i as u16) * 7) % cols;
        tracker.mark_cell(row, col);
    }
    tracker
}

fn patch_diff_apply_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("patch_diff_apply");

    let cols = 120;
    let rows = 40;
    let scenarios = [
        ("1_cell", 1usize),
        ("10_cells", 10usize),
        ("200_cells", 200usize),
        ("2000_cells", 2000usize),
    ];

    for (id, changes) in scenarios {
        let (old, new) = make_old_new_grid(cols, rows, changes);
        let tracker = make_dirty_tracker(cols, rows, changes);

        group.bench_function(BenchmarkId::new("diff_alloc", id), |b| {
            b.iter(|| {
                let patch = GridDiff::diff(black_box(&old), black_box(&new));
                black_box(patch.len());
            });
        });

        group.bench_function(BenchmarkId::new("diff_reuse", id), |b| {
            let mut patch = Patch::new(cols, rows);
            b.iter(|| {
                GridDiff::diff_into(black_box(&old), black_box(&new), &mut patch);
                black_box(patch.len());
            });
        });

        group.bench_function(BenchmarkId::new("diff_dirty", id), |b| {
            b.iter(|| {
                let patch =
                    GridDiff::diff_dirty(black_box(&old), black_box(&new), black_box(&tracker));
                black_box(patch.len());
            });
        });

        // Apply cost (without cloning): forward patch then reverse patch each iter.
        let forward = GridDiff::diff(&old, &new);
        let backward = GridDiff::diff(&new, &old);
        let updates_per_iter = (forward.len() + backward.len()) as u64;
        group.throughput(Throughput::Elements(updates_per_iter));

        group.bench_function(BenchmarkId::new("apply_forward_and_back", id), |b| {
            let mut grid = old.clone();
            b.iter(|| {
                apply_patch(&mut grid, black_box(&forward));
                apply_patch(&mut grid, black_box(&backward));
                black_box(grid.cell(0, 0).map(Cell::content));
            });
        });
    }

    group.finish();
}

fn parser_action_mix_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser_action_mix");

    // A small action-heavy stream that produces a mix of Action variants.
    let stream = b"ab\x08c\tZ\x1b[2;3HX\x1b[2J\x1b[1;4H\x1b[0K!\n";
    group.throughput(Throughput::Bytes(stream.len() as u64));

    group.bench_function("advance_count_actions", |b| {
        let mut parser = Parser::new();
        b.iter(|| {
            let mut counts = [0u64; 4];
            for &b in black_box(stream) {
                if let Some(action) = parser.advance(b) {
                    match action {
                        Action::Print(_) => counts[0] += 1,
                        Action::Newline
                        | Action::CarriageReturn
                        | Action::Tab
                        | Action::Backspace => counts[1] += 1,
                        Action::EraseInDisplay(_)
                        | Action::EraseInLine(_)
                        | Action::CursorPosition { .. } => counts[2] += 1,
                        _ => counts[3] += 1,
                    }
                }
            }
            black_box(counts);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    parser_throughput_bench,
    parser_action_mix_bench,
    patch_diff_apply_bench
);
criterion_main!(benches);
