use std::hint::black_box;
use std::mem::size_of;
use std::process::Command;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use frankenterm_core::{
    Action, Cell, Color, DirtyTracker, Grid, GridDiff, Parser, Patch, Scrollback, ScrollbackLine,
    SgrAttrs,
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

/// Generate larger corpora by repeating base patterns to target ~64 KB.
/// These give more stable throughput measurements than the small corpora.
fn large_corpora() -> Vec<(&'static str, Vec<u8>)> {
    // Colored compiler output: dense SGR color switches with text.
    let sgr_line = b"\x1b[1;32m   Compiling\x1b[0m frankenterm-core v0.1.0 \
\x1b[2m(/repo/crates/frankenterm-core)\x1b[0m\r\n\
\x1b[1;33mwarning\x1b[0m: unused variable `\x1b[1mx\x1b[0m`\r\n\
 \x1b[1;34m-->\x1b[0m src/lib.rs:42:9\r\n";
    let sgr_stream = sgr_line.repeat(64 * 1024 / sgr_line.len());

    // Cursor-heavy stream: simulating ncurses-like full-screen updates.
    let cursor_line = b"\x1b[1;1H\x1b[2J\x1b[1;1HABCDEFGHIJ\
\x1b[2;1HKLMNOPQRST\x1b[3;1H0123456789\
\x1b[1;5H\x1b[0K\x1b[3;8H\x1b[1P\x1b[2;3H\x1b[2@  ";
    let cursor_stream = cursor_line.repeat(64 * 1024 / cursor_line.len());

    // UTF-8 mixed content: CJK + emoji + Latin accents + ASCII.
    let utf8_line = "你好世界 café résumé — 🦀🔥✅ line of text 日本語テスト\r\n".as_bytes();
    let utf8_stream = utf8_line.repeat(64 * 1024 / utf8_line.len());

    // Plain ASCII: best-case throughput baseline.
    let ascii_line = b"The quick brown fox jumps over the lazy dog. 0123456789 ABCDEF\r\n";
    let ascii_stream = ascii_line.repeat(64 * 1024 / ascii_line.len());

    vec![
        ("sgr_64k_v1", sgr_stream),
        ("cursor_64k_v1", cursor_stream),
        ("utf8_64k_v1", utf8_stream),
        ("ascii_64k_v1", ascii_stream),
    ]
}

fn make_row(cols: u16, seed: u32) -> Vec<Cell> {
    (0..cols)
        .map(|col| {
            let mut cell = Cell::new((b'a' + ((seed + u32::from(col)) % 26) as u8) as char);
            cell.attrs = SgrAttrs {
                fg: Color::Named(((seed + u32::from(col)) % 16) as u8),
                bg: Color::Default,
                ..SgrAttrs::default()
            };
            cell
        })
        .collect()
}

fn build_scrollback(lines: usize, cols: u16) -> Scrollback {
    let mut scrollback = Scrollback::new(lines);
    for i in 0..lines {
        let row = make_row(cols, i as u32);
        let _ = scrollback.push_row(&row, i % 3 == 0);
    }
    scrollback
}

/// Lower-bound estimate of scrollback heap footprint.
///
/// This excludes VecDeque spare capacity overhead and allocator metadata,
/// but it is deterministic and useful for CI regression tracking.
fn estimate_scrollback_heap_bytes(scrollback: &Scrollback) -> usize {
    let line_headers = scrollback.len() * size_of::<ScrollbackLine>();
    let cell_storage: usize = scrollback
        .iter()
        .map(|line| line.cells.capacity() * size_of::<Cell>())
        .sum();
    line_headers + cell_storage
}

fn scrollback_memory_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("scrollback_memory");
    let line_count = 1_000usize;

    for cols in [80u16, 120u16, 200u16] {
        let scrollback = build_scrollback(line_count, cols);
        let bytes = estimate_scrollback_heap_bytes(&scrollback);
        eprintln!(
            "{{\"event\":\"scrollback_memory\",\"lines\":{},\"cols\":{},\"heap_bytes\":{},\"bytes_per_line\":{}}}",
            line_count,
            cols,
            bytes,
            bytes / line_count
        );

        let id = format!("estimate_bytes_1k_{}cols", cols);
        group.bench_function(BenchmarkId::from_parameter(id), |b| {
            b.iter(|| {
                let est = estimate_scrollback_heap_bytes(black_box(&scrollback));
                black_box(est);
            });
        });
    }

    group.finish();
}

fn scrollback_virtualization_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("scrollback_virtualization");

    let line_count = 100_000usize;
    let cols = 120u16;
    let viewport_lines = 40usize;
    let overscan_lines = 8usize;
    let scrollback = build_scrollback(line_count, cols);
    let offsets = [
        0usize,
        64usize,
        512usize,
        line_count / 4,
        line_count / 2,
        line_count.saturating_sub(viewport_lines),
    ];

    let sample_window = scrollback.virtualized_window(offsets[3], viewport_lines, overscan_lines);
    eprintln!(
        "{{\"event\":\"scrollback_virtualization\",\"lines\":{},\"cols\":{},\"viewport_lines\":{},\"overscan_lines\":{},\"render_lines\":{},\"max_scroll_offset\":{}}}",
        line_count,
        cols,
        viewport_lines,
        overscan_lines,
        sample_window.render_len(),
        sample_window.max_scroll_offset
    );

    group.throughput(Throughput::Elements(sample_window.render_len() as u64));
    group.bench_function(BenchmarkId::from_parameter("window_compute"), |b| {
        b.iter(|| {
            let mut checksum = 0usize;
            for &offset in &offsets {
                let window = scrollback.virtualized_window(offset, viewport_lines, overscan_lines);
                checksum ^= window.viewport_start;
                checksum ^= window.render_end;
            }
            black_box(checksum);
        });
    });

    group.bench_function(
        BenchmarkId::from_parameter("iter_virtualized_window"),
        |b| {
            b.iter(|| {
                let window =
                    scrollback.virtualized_window(line_count / 2, viewport_lines, overscan_lines);
                let mut cells = 0usize;
                for line in scrollback.iter_range(window.render_range()) {
                    cells = cells.saturating_add(line.cells.len());
                }
                black_box(cells);
            });
        },
    );

    group.bench_function(BenchmarkId::from_parameter("iter_full_history"), |b| {
        b.iter(|| {
            let mut cells = 0usize;
            for line in scrollback.iter() {
                cells = cells.saturating_add(line.cells.len());
            }
            black_box(cells);
        });
    });

    group.finish();
}

fn seed_grid(grid: &mut Grid) {
    for row in 0..grid.rows() {
        for col in 0..grid.cols() {
            if (u32::from(row) + u32::from(col)) % 11 == 0
                && let Some(cell) = grid.cell_mut(row, col)
            {
                let mut seeded = Cell::new((b'A' + ((row + col) % 26) as u8) as char);
                seeded.attrs = SgrAttrs {
                    fg: Color::Named(((row + col) % 16) as u8),
                    bg: Color::Default,
                    ..SgrAttrs::default()
                };
                *cell = seeded;
            }
        }
    }
}

fn run_resize_storm(
    start_cols: u16,
    start_rows: u16,
    pattern: &[(u16, u16)],
    cycles: usize,
) -> (u16, usize, usize) {
    let mut grid = Grid::new(start_cols, start_rows);
    seed_grid(&mut grid);
    let mut scrollback = build_scrollback(1_000, start_cols);
    let mut cursor_row = (start_rows / 2).min(start_rows.saturating_sub(1));

    for _ in 0..cycles {
        for &(cols, rows) in pattern {
            let max_row = grid.rows().saturating_sub(1);
            cursor_row = cursor_row.min(max_row);
            cursor_row = grid.resize_with_scrollback(cols, rows, cursor_row, &mut scrollback);
        }
    }

    (
        cursor_row,
        scrollback.len(),
        estimate_scrollback_heap_bytes(&scrollback),
    )
}

type ResizeScenario = (&'static str, u16, u16, &'static [(u16, u16)], usize);

fn resize_storm_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("resize_storm");

    let scenarios: [ResizeScenario; 2] = [
        (
            "120x40_120x52",
            120,
            40,
            &[(120, 52), (120, 40), (120, 56), (120, 40)],
            25,
        ),
        (
            "80x24_200x60",
            80,
            24,
            &[(120, 40), (160, 50), (200, 60), (80, 24)],
            20,
        ),
    ];

    for (id, start_cols, start_rows, pattern, cycles) in scenarios {
        let events = pattern.len() * cycles;
        eprintln!(
            "{{\"event\":\"resize_storm\",\"id\":\"{}\",\"start_cols\":{},\"start_rows\":{},\"events\":{}}}",
            id, start_cols, start_rows, events
        );
        group.throughput(Throughput::Elements(events as u64));
        group.bench_function(BenchmarkId::new("resize_with_scrollback", id), |b| {
            b.iter(|| {
                let result = run_resize_storm(start_cols, start_rows, pattern, cycles);
                black_box(result);
            });
        });
    }

    group.finish();
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

fn parser_throughput_large_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser_throughput_large");
    for (id, bytes) in large_corpora() {
        let hash = fnv1a64(&bytes);
        eprintln!(
            "[frankenterm-core bench] corpus={} bytes={} fnv1a64={:016x}",
            id,
            bytes.len(),
            hash
        );

        group.throughput(Throughput::Bytes(bytes.len() as u64));

        group.bench_with_input(BenchmarkId::new("feed_vec", id), &bytes, |b, bytes| {
            let mut parser = Parser::new();
            b.iter(|| {
                let actions = parser.feed(black_box(bytes));
                black_box(actions.len());
            });
        });

        group.bench_with_input(BenchmarkId::new("advance_count", id), &bytes, |b, bytes| {
            let mut parser = Parser::new();
            b.iter(|| {
                let mut count = 0u64;
                for &byte in black_box(bytes.as_slice()) {
                    if parser.advance(byte).is_some() {
                        count += 1;
                    }
                }
                black_box(count);
            });
        });
    }
    group.finish();
}

fn full_pipeline_bench(c: &mut Criterion) {
    use frankenterm_core::Cursor;

    let mut group = c.benchmark_group("full_pipeline");
    for (id, bytes) in large_corpora() {
        group.throughput(Throughput::Bytes(bytes.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("parse_and_apply", id),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let mut parser = Parser::new();
                    let mut grid = Grid::new(120, 40);
                    let mut cursor = Cursor::new(120, 40);
                    let mut scrollback = Scrollback::new(512);

                    for action in parser.feed(black_box(bytes)) {
                        match action {
                            Action::Print(ch) => {
                                if cursor.pending_wrap {
                                    cursor.col = 0;
                                    if cursor.row + 1 >= cursor.scroll_bottom() {
                                        grid.scroll_up_into(
                                            cursor.scroll_top(),
                                            cursor.scroll_bottom(),
                                            1,
                                            &mut scrollback,
                                            cursor.attrs.bg,
                                        );
                                    } else if cursor.row + 1 < 40 {
                                        cursor.row += 1;
                                    }
                                    cursor.pending_wrap = false;
                                }

                                let width = Cell::display_width(ch);
                                if width == 0 {
                                    continue;
                                }
                                if width == 2 && cursor.col + 1 >= 120 {
                                    cursor.col = 0;
                                    if cursor.row + 1 >= cursor.scroll_bottom() {
                                        grid.scroll_up_into(
                                            cursor.scroll_top(),
                                            cursor.scroll_bottom(),
                                            1,
                                            &mut scrollback,
                                            cursor.attrs.bg,
                                        );
                                    } else if cursor.row + 1 < 40 {
                                        cursor.row += 1;
                                    }
                                }

                                let written =
                                    grid.write_printable(cursor.row, cursor.col, ch, cursor.attrs);
                                if written == 0 {
                                    continue;
                                }

                                if cursor.col + u16::from(written) >= 120 {
                                    cursor.pending_wrap = true;
                                } else {
                                    cursor.col += u16::from(written);
                                }
                            }
                            Action::Newline => {
                                if cursor.row + 1 >= cursor.scroll_bottom() {
                                    grid.scroll_up_into(
                                        cursor.scroll_top(),
                                        cursor.scroll_bottom(),
                                        1,
                                        &mut scrollback,
                                        cursor.attrs.bg,
                                    );
                                } else if cursor.row + 1 < 40 {
                                    cursor.row += 1;
                                }
                                cursor.pending_wrap = false;
                            }
                            Action::CarriageReturn => cursor.carriage_return(),
                            Action::CursorPosition { row, col } => {
                                cursor.move_to(row, col, 40, 120);
                            }
                            Action::CursorUp(n) => cursor.move_up(n),
                            Action::CursorDown(n) => cursor.move_down(n, 40),
                            Action::CursorRight(n) => cursor.move_right(n, 120),
                            Action::CursorLeft(n) => cursor.move_left(n),
                            Action::EraseInDisplay(mode) => match mode {
                                0 => grid.erase_below(cursor.row, cursor.col, Color::Default),
                                1 => grid.erase_above(cursor.row, cursor.col, Color::Default),
                                2 => grid.erase_all(Color::Default),
                                _ => {}
                            },
                            Action::EraseInLine(mode) => match mode {
                                0 => grid.erase_line_right(cursor.row, cursor.col, Color::Default),
                                1 => grid.erase_line_left(cursor.row, cursor.col, Color::Default),
                                2 => grid.erase_line(cursor.row, Color::Default),
                                _ => {}
                            },
                            Action::Sgr(params) => cursor.attrs.apply_sgr_params(&params),
                            Action::ScrollUp(n) => grid.scroll_up_into(
                                cursor.scroll_top(),
                                cursor.scroll_bottom(),
                                n,
                                &mut scrollback,
                                cursor.attrs.bg,
                            ),
                            Action::ScrollDown(n) => grid.scroll_down(
                                cursor.scroll_top(),
                                cursor.scroll_bottom(),
                                n,
                                cursor.attrs.bg,
                            ),
                            Action::InsertChars(n) => {
                                grid.insert_chars(cursor.row, cursor.col, n, Color::Default);
                            }
                            Action::DeleteChars(n) => {
                                grid.delete_chars(cursor.row, cursor.col, n, Color::Default);
                            }
                            _ => {}
                        }
                    }
                    black_box(grid.cell(0, 0).map(Cell::content));
                });
            },
        );
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
    scrollback_memory_bench,
    scrollback_virtualization_bench,
    resize_storm_bench,
    parser_throughput_large_bench,
    full_pipeline_bench,
    parser_action_mix_bench,
    patch_diff_apply_bench
);
criterion_main!(benches);
