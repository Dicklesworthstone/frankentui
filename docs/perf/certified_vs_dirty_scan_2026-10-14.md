# Certified vs dirty-span/SAT vs full diff scans (2026-10-14)

Bead `bd-g00-root-epic-ewths.48.1` (G49). Question: what does the diff scan
that runs by default cost, next to the scans README describes?

## What runs by default

`TerminalWriter` defaults to `certified_skips: true`. Under
`DiffStrategy::DirtyRows` it builds a render certificate from
`buffer.dirty_row_indices()` and calls `compute_certified_into(.., NarrowToRows)`,
which compares each dirty row as a slice and, when the row differs, every cell
of it. The dirty-span list and the summed-area-table (SAT) tile skip are only
used by `compute_dirty_into`, which runs when `certified_skips` is false.

## Method

`crates/ftui-render/benches/diff_paths_paired.rs` (`cargo bench -p ftui-render
--bench diff_paths_paired`), one invocation, four paths on the same buffer pair:

| path | what it calls |
|---|---|
| `certified` | the production sequence: `evaluate_render_certificate` over `dirty_row_indices()`, then `compute_certified_into(.., to_hint())` |
| `dirty` | `compute_dirty_into` (dirty spans + SAT tile skip) |
| `full` | `compute_into` |
| `certified_present` | `certified` + `Presenter::present` into a `Vec` (diff + emit), timed per frame |

Every path's change list is checked against `full` before timing. Each of 41
rounds times one batch (~0.3 ms) of every path in an order rotated per round;
ratios are of medians with a paired 95% bootstrap interval (2,000 resamples).
`new` carries dirty marks for exactly the cells written, the best case for
`certified` and `dirty` (a real frame marks every cell a widget wrote).
Profile `bench`, i.e. the workspace release settings (`opt-level = "z"`).

Host: ts1 (`thinkstation1`), AMD Threadripper PRO 5975WX (32 cores / 64
threads), governor `powersave`, load 15 (run 1) and 6 (run 2),
`nightly-2026-08-31`, source `4b462e07` plus the bench file. Raw rows:
[`certified_vs_dirty_scan_2026-10-14.jsonl`](certified_vs_dirty_scan_2026-10-14.jsonl)
(run 2, which also records the strategy selector's prediction per row).

**Variance.** The winning path is the same in all 36 workloads in both runs,
but magnitudes move by up to 29% between runs (e.g. spinner at 120x40:
certified/dirty 4.88 then 3.46). The within-run intervals below are tight and
understate run-to-run variance; read the ratios to one significant figure.

## Results (run 2, p50 ns per call)

`c/d` = certified ÷ dirty (below 1: the default path is faster); `c/f` =
certified ÷ full. `fallback` is the dirty path's tile decision (`small_screen`:
no SAT below its size threshold, spans only).

| size | pattern | changed | certified | dirty | full | c/d | c/f | fallback |
|---|---|---:|---:|---:|---:|---:|---:|---|
| 80x24 | single_cell | 1 | 179 | 73 | 2967 | 2.44 | 0.06 | small_screen |
| 80x24 | scattered_5pct | 96 | 2287 | 730 | 2854 | 3.13 | 0.80 | small_screen |
| 80x24 | rows_5pct | 160 | 485 | 552 | 3164 | 0.88 | 0.15 | small_screen |
| 80x24 | spinner_tick | 1 | 157 | 39 | 2874 | 4.02 | 0.06 | small_screen |
| 80x24 | status_line | 30 | 209 | 143 | 2957 | 1.46 | 0.07 | small_screen |
| 80x24 | full_100pct | 1920 | 5298 | 6437 | 5734 | 0.82 | 0.92 | small_screen |
| 120x40 | single_cell | 1 | 268 | 102 | 7090 | 2.62 | 0.04 | small_screen |
| 120x40 | scattered_5pct | 240 | 5641 | 1786 | 7211 | 3.16 | 0.78 | small_screen |
| 120x40 | spinner_tick | 1 | 203 | 59 | 7074 | 3.46 | 0.03 | small_screen |
| 120x40 | status_line | 30 | 277 | 162 | 7204 | 1.71 | 0.04 | small_screen |
| 200x60 | single_cell | 1 | 384 | 805 | 17801 | 0.48 | 0.02 | none |
| 200x60 | scattered_5pct | 600 | 13161 | 9121 | 17848 | 1.44 | 0.74 | dense_tiles |
| 200x60 | one_full_row | 200 | 581 | 1517 | 18114 | 0.38 | 0.03 | none |
| 200x60 | status_line | 30 | 379 | 876 | 17918 | 0.43 | 0.02 | none |
| 300x100 | scattered_5pct | 1500 | 38587 | 22123 | 44713 | 1.74 | 0.86 | dense_tiles |
| 300x100 | spinner_tick | 1 | 421 | 1672 | 43593 | 0.25 | 0.01 | none |
| 300x100 | full_100pct | 30000 | 74518 | 92224 | 84305 | 0.81 | 0.88 | dense_cells |

(Selected rows; all 36 with intervals, both runs' ratios agree in direction,
are in the JSONL.)

## Findings

1. **The default path is never slower than a full scan** (c/f 0.01–0.92), so
   choosing DirtyRows is always right on these workloads.
2. **Against the dirty-span scan it loses where interactive UIs live:** at
   80x24 and 120x40, a spinner, a cursor-sized change or a status-line update
   costs 1.5–4.0× more on the default path, because it compares whole rows
   where the span scan touches the written cells. Scattered changes cost
   3.1–3.2× more at those sizes and 1.4–1.7× more at 200x60/300x100 (where the
   dirty path falls back from tiles to spans).
3. **The SAT costs more than it saves on these workloads:** at 200x60 and
   300x100, wherever the tile path runs (`fallback: none`), the default path is
   2–4× faster than the dirty path (c/d 0.25–0.51). Building the SAT over the
   whole screen is the dirty path's cost; the tile skip does not pay it back
   here.
4. **Dense and whole-row changes favour the default path:** c/d 0.75–0.83 for
   dense, full and scrolled frames, 0.85–0.96 for whole-row changes at the
   small sizes; the span bookkeeping is overhead when spans cover the row.
5. **The strategy selector prices a scan that does not run.** It is fed span
   coverage (or the tile scan estimate) as the DirtyRows scan estimate. On the
   sparse workloads the default path scans 1.6× to 240× more cells than that
   estimate (it reads whole dirty rows: slice compare plus per-cell pass). Predicted vs measured advantage
   over full for scattered 5% at 80x24: `cost_full/cost_dirty` = 20×; measured
   `full/certified` = 1.25×. Its choice is still DirtyRows in every row (fresh
   selector, `guard: uncertainty_variance`).

## Plan budgets (founding plan §0.12.1, diff + emit, release profile)

`certified_present` per frame, scattered 5% (budgets: p50 / p99):

| size | p50 | p99 | budget | result |
|---|---:|---:|---|---|
| 80x24 | 11.3 µs | 18.6 µs | 1.0 ms / 3.0 ms | pass |
| 120x40 | 28.1 µs | 37.8 µs | 2.0 ms / 6.0 ms | pass |
| 200x60 | 71.3 µs | 83.9 µs | 6.0 ms / 18.0 ms | pass |

(A fresh `Presenter` per frame, so slightly pessimistic.) Bytes per frame are
O(changes): spinner 44–45, status line 74–75, single cell 45–46 bytes at every
size.

## Decision input for `.48.2`

Neither shipped scan dominates. A narrow scan that walks each dirty row's
**dirty spans** (no SAT build) should take the default path's wins at large
sizes (no whole-screen SAT) and the span scan's wins at small sizes and for
sparse rows, while keeping whole-row comparison when a row's span is full. The
measurement for that must keep today's default path live in the same
invocation as the incumbent.

## Not measured

- Cells compared are counted analytically for the default path (exact for
  its loop: slice compare per dirty row plus a per-cell pass over rows that
  differ) and taken from the dirty path's own `scan_cells_estimate`; no
  instrumented counter was added to `diff.rs`.
- Realistic dirty marking (widgets redrawing unchanged cells), which widens
  spans and dirty rows for both narrow paths.
- Any host but ts1; `powersave` governor, no core pinning.
