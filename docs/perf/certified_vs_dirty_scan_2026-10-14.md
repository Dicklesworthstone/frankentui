# Certified vs incumbent row-slice vs dirty-span/SAT vs full diff scans (2026-10-14)

Beads `bd-g00-root-epic-ewths.48.1` and `bd-g00-root-epic-ewths.48.2` (G49).

## What runs by default (after `.48.2`)

`TerminalWriter` defaults to `certified_skips: true`. Under `DiffStrategy::DirtyRows` it evaluates an independent render certificate (`evaluate_render_certificate_for_buffers`) that cross-checks:

1. **Baseline generation lineage (`baseline_valid`)**: `new.dirty_baseline_gen() == Some(old.mutation_gen())` (or `new.dirty_baseline_gen().is_none()` when all rows are marked full-dirty).
2. **Dual-witness agreement (`span_dirty_rows == dirty_row_count`)**: `new.dirty_rows` bitmap agrees row-for-row with `new.dirty_spans[y].is_dirty()`.
3. **Strict monotonicity and bounds**: `dirty_rows` is strictly ascending and `< total_rows`.

It then calls `BufferDiff::compute_certified_into(old, new, cert.into_hint())`:
- `CertHint::SkipDiff` -> empty diff (`O(1)`).
- `CertHint::NarrowToRows(rows)` -> walks each certified dirty row using `new.dirty_span_row(y)` (`scan_row_changes_spans` when spans are tracked, or `scan_row_changes` when the row is marked full-width dirty), with zero heap allocation when `rows` is already strictly ascending and zerowhole-screen SAT construction overhead.
- `CertHint::FullDiff` -> delegates directly to `self.compute_into(old, new)`.

The pre-`.48.2` default path (`BufferDiff::compute_certified_row_slice_into`) stays compiled in the same struct so the paired benchmark compares the new default path (`certified`) against the incumbent (`incumbent_row_slice`), `compute_dirty_into` (`dirty`), and `compute_into` (`full`) in the same invocation.

## Method

`crates/ftui-render/benches/diff_paths_paired.rs` (`cargo bench -p ftui-render --bench diff_paths_paired`), one invocation, five paths on the same buffer pair across 4 sizes (`80x24`, `120x40`, `200x60`, `300x100`) and 10 patterns (including `overmarked_chrome`, where border/header/status widgets redraw identical cells while only 4 spinner/counter cells actually differ):

| path | what it calls |
|---|---|
| `certified` | the production sequence: `evaluate_render_certificate_for_buffers(Some(old), new, false)`, then `compute_certified_into(old, new, cert.into_hint())` |
| `incumbent_row_slice` | the pre-`.48.2` sequence: `evaluate_render_certificate` + `compute_certified_row_slice_into` (whole-row slice equality + full row scan) |
| `dirty` | `compute_dirty_into` (dirty spans + summed-area-table tile skip) |
| `full` | `compute_into` (block/SIMD row skip + scalar scan) |
| `certified_present` | `certified` + `Presenter::present_with_pool` into a `Vec` (diff + emit), timed per frame |

Every path's change list is checked for exact equality against `full` before timing, and every path records an instrumented cell comparison count (`BufferDiff::last_scanned_cells()`). Each of 41 rounds times one batch (~0.3 ms) of every path in an order rotated per round; ratios are of medians with a paired 95% bootstrap interval (2,000 resamples). Profile `bench` (`opt-level = "z"`). Raw rows: [`certified_vs_dirty_scan_2026-10-14.jsonl`](certified_vs_dirty_scan_2026-10-14.jsonl).

## Results (p50 ns per call, all 4 sizes)

`c/inc` = `certified ÷ incumbent_row_slice` (below 1: new default beats pre-`.48.2` default); `c/d` = `certified ÷ dirty`; `c/f` = `certified ÷ full`. `scan (c/inc)` = instrumented cells compared by `certified` vs `incumbent_row_slice`.

| size | pattern | changed | scan (c / inc) | certified | incumbent | dirty | full | c/inc | c/d | c/f |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 80x24 | single_cell | 1 | 1 / 160 | 71 | 211 | 30 | 2006 | 0.34 | 2.35 | 0.04 |
| 80x24 | scattered_5pct | 96 | 96 / 3840 | 650 | 2717 | 564 | 2366 | 0.24 | 1.15 | 0.28 |
| 80x24 | rows_5pct | 160 | 160 / 320 | 641 | 716 | 697 | 2849 | 0.90 | 0.92 | 0.23 |
| 80x24 | spinner_tick | 1 | 1 / 160 | 68 | 170 | 29 | 1941 | 0.40 | 2.31 | 0.04 |
| 80x24 | status_line | 30 | 30 / 160 | 138 | 228 | 107 | 2032 | 0.60 | 1.30 | 0.07 |
| 80x24 | overmarked_chrome | 4 | 40 / 720 | 166 | 789 | 123 | 2013 | 0.21 | 1.35 | 0.08 |
| 80x24 | full_100pct | 1920 | 1920 / 3840 | 6046 | 5961 | 5734 | 6045 | 1.01 | 1.05 | 1.00 |
| 120x40 | single_cell | 1 | 1 / 240 | 136 | 446 | 53 | 6171 | 0.31 | 2.56 | 0.02 |
| 120x40 | scattered_5pct | 240 | 240 / 9600 | 1979 | 10891 | 1991 | 7508 | 0.18 | 0.99 | 0.26 |
| 120x40 | spinner_tick | 1 | 1 / 240 | 91 | 228 | 38 | 4828 | 0.40 | 2.37 | 0.02 |
| 120x40 | status_line | 30 | 30 / 240 | 160 | 297 | 113 | 4875 | 0.54 | 1.42 | 0.03 |
| 120x40 | overmarked_chrome | 4 | 48 / 1560 | 251 | 1676 | 152 | 4829 | 0.15 | 1.66 | 0.05 |
| 120x40 | full_100pct | 4800 | 4800 / 9600 | 12442 | 12476 | 13135 | 12135 | 1.00 | 0.95 | 1.03 |
| 200x60 | single_cell | 1 | 1 / 400 | 129 | 423 | 926 | 12124 | 0.30 | 0.14 | 0.01 |
| 200x60 | scattered_5pct | 600 | 600 / 24000 | 2936 | 14685 | 8039 | 13337 | 0.20 | 0.37 | 0.22 |
| 200x60 | one_full_row | 200 | 200 / 400 | 630 | 622 | 1610 | 12403 | 1.01 | 0.39 | 0.05 |
| 200x60 | spinner_tick | 1 | 1 / 400 | 126 | 350 | 943 | 12316 | 0.36 | 0.13 | 0.01 |
| 200x60 | status_line | 30 | 30 / 400 | 192 | 421 | 1040 | 12498 | 0.46 | 0.19 | 0.02 |
| 200x60 | overmarked_chrome | 4 | 58 / 3600 | 365 | 3824 | 1621 | 12640 | 0.10 | 0.23 | 0.03 |
| 200x60 | full_100pct | 12000 | 12000 / 24000 | 30898 | 30898 | 32627 | 30453 | 1.00 | 0.95 | 1.02 |
| 300x100 | single_cell | 1 | 1 / 600 | 195 | 627 | 2088 | 30979 | 0.31 | 0.09 | 0.01 |
| 300x100 | scattered_5pct | 1500 | 1500 / 60000 | 7028 | 36617 | 20170 | 33757 | 0.19 | 0.35 | 0.21 |
| 300x100 | one_full_row | 300 | 300 / 600 | 957 | 919 | 3096 | 31624 | 1.04 | 0.31 | 0.03 |
| 300x100 | spinner_tick | 1 | 1 / 600 | 193 | 499 | 2084 | 31238 | 0.39 | 0.09 | 0.01 |
| 300x100 | status_line | 30 | 30 / 600 | 279 | 623 | 2217 | 31592 | 0.45 | 0.13 | 0.01 |
| 300x100 | overmarked_chrome | 4 | 78 / 8400 | 536 | 8654 | 3212 | 31949 | 0.06 | 0.17 | 0.02 |
| 300x100 | full_100pct | 30000 | 30000 / 60000 | 77537 | 75751 | 81740 | 75584 | 1.02 | 0.95 | 1.03 |

## Findings

1. **`certified` beats `incumbent_row_slice` by 1.7× to 16.2× on sparse and over-marked interactive frames (`c/inc` 0.062–0.604) and ties (`c/inc` 0.90–1.04) on dense/full frames:**
   - On `overmarked_chrome` (widgets redrawing border/header/status chrome while 4 cells actually change), `certified` scans only the dirty spans (`40..78` cells) instead of full rows (`720..8,400` cells), running **4.7× faster at 80×24** (`166 ns` vs `789 ns`), **6.7× faster at 120×40** (`251 ns` vs `1,676 ns`), **10.5× faster at 200×60** (`365 ns` vs `3,824 ns`), and **16.2× faster at 300×100** (`536 ns` vs `8,654 ns`).
   - On `scattered_5pct`, `certified` runs **4.2× to 5.5× faster** than `incumbent_row_slice` across all sizes (`c/inc` `0.182–0.239`).
   - On `single_cell`, `spinner_tick`, and `status_line`, `certified` runs **1.7× to 3.3× faster** than `incumbent_row_slice` (`c/inc` `0.304–0.604`).
   - On whole-row and full-screen workloads (`rows_5pct`, `one_full_row`, `log_scroll`, `dense_50pct`, `full_100pct`), `certified` matches `incumbent_row_slice` (`c/inc` `0.895–1.041`) while scanning half as many cells (`width` instead of `2 * width` per dirty row because the redundant pre-scan `old_row == new_row` before cell emission is eliminated).
2. **At 200×60 and 300×100, `certified` beats `dirty` (`compute_dirty_into`) by 1.8× to 10.8× on every sparse and partial-row workload (`c/d` 0.093–0.550):**
   - `compute_dirty_into` builds a whole-screen summed-area table (SAT) at `width * height >= 12,000` (`120x40` is 4,800 cells, so `small_screen`; `200x60` is 12,000 cells, so SAT runs). At 300×100, `spinner_tick` costs `193 ns` on `certified` vs `2,084 ns` on `dirty` (`c/d = 0.093`).
   - At 80×24 and 120×40 (`small_screen`), `certified` pays ~40–80 ns for single-pass witness collection + certificate validation (`collect_dirty_witness_for` + `evaluate_render_certificate_for_buffers`), so 1-cell updates take `68–136 ns` vs `29–53 ns` on raw `compute_dirty_into`, while `scattered_5pct` and `rows_5pct` tie (`c/d` `0.92–1.15`).
3. **Strategy selector scan-cell alignment is now exact (`certified_scan_cells == span_coverage_cells` across all 40 workloads):**
   - Before `.48.2`, the selector priced `DirtyRows` using `span_coverage_cells`, while `compute_certified_into` scanned `1.6×` to `600×` more cells (`incumbent_row_slice_scan_cells`). Now `certified_scan_cells == span_coverage_cells` in all 40 rows (`1 == 1` on `single_cell`/`spinner_tick`, `30 == 30` on `status_line`, `40..78 == 40..78` on `overmarked_chrome`, `1,920..30,000` on `full_100pct`).

## Plan budgets (founding plan §0.12.1, diff + emit, release profile)

`certified_present` per frame, scattered 5% (budgets: p50 / p99):

| size | p50 | p99 | budget | result |
|---|---:|---:|---|---|
| 80x24 | 10.2 µs | 16.3 µs | 1.0 ms / 3.0 ms | pass |
| 120x40 | 35.0 µs | 41.7 µs | 2.0 ms / 6.0 ms | pass |
| 200x60 | 58.6 µs | 61.9 µs | 6.0 ms / 18.0 ms | pass |
| 300x100 | 145.6 µs | 160.3 µs | — | — |
