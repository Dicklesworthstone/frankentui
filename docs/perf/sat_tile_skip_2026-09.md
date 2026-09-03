# SAT tile-row prefilter — measurements (2026-09)

Bead: `bd-g00-root-epic-ewths.19.2` (G28). This records what the summed-area
table (SAT) buys once it is actually queried. Before this change the SAT was
built on every tiled diff and never read — skipping used a per-tile `bool`
grid, so the SAT was pure overhead. The change adds `TileDiffPlan::tile_row_dirty`
(O(1) row sum) and `rect_dirty` (O(1) any-rectangle), and makes the tiled scan
driver retire a whole clean tile row with one subtraction before touching any
buffer row in it. New stats `skipped_tile_rows` and `sat_queries` are exported
in `TileDiffStats` and the `diff_decision` JSONL row.

## Method

- Bench: `crates/ftui-render/benches/diff_bench.rs`, group `diff/sparse_5pct_rows`
  (added by this bead). `make_pair_rows` puts 5% of the cells dirty but confines
  them to three tile rows (bands of `tile_h = 8` buffer rows) — the case the
  prefilter targets (a status line, a scrolling log): sparse overall, clustered
  in a few bands, so most tile rows are entirely clean.
- Two methods per size: `compute` (the flat row diff, `BufferDiff::compute`,
  which has no tile path at all) and `compute_dirty` (the tile path, now with
  the SAT row prefilter). The `compute_dirty` config disables the small-screen
  and dense-tile fallbacks so the tile path engages at every listed size and the
  prefilter is what is being exercised.
- Command:
  `cargo bench -p ftui-render --bench diff_bench -- 'diff/sparse_5pct_rows' --measurement-time 3 --warm-up-time 1 --sample-size 30`
- Machine: rch remote build/bench worker (shared, timing is noisy — treat the
  microsecond figures as indicative, not authoritative). Built at parent commit
  `c9192ce6` plus this bead's changes.

## Results (p50, most-stable sample)

| size    | flat `compute` p50 | tile + prefilter p50 | tile rows retired | `sat_queries` | fallback |
|---------|--------------------|----------------------|-------------------|---------------|----------|
| 120x40  | 10 us              | 5 us                 | 2 of 5            | 5             | none     |
| 200x60  | 28 us              | 21 us                | 5 of 8            | 8             | none     |
| 240x80  | 43 us              | 10 us                | 7 of 10           | 10            | none     |

`skipped_tile_rows` / `tiles_y` = 2/5, 5/8, 7/10: exactly the tile rows outside
the three dirty bands, each retired by a single SAT subtraction. `sat_queries`
equals `tiles_y` (one row-sum query per tile row). `scanned_tiles` equals
`dirty_tiles` (24, 39, 45), i.e. only the tiles inside the dirty bands.

Delta for the decision rule, `diff/sparse_5pct_rows/200x60`: tile + prefilter
21 us vs flat compute 28 us, about 25% faster on this concentrated case, while
retiring 5 of 8 tile rows with the SAT.

## Honest scope and caveats

- **This is tile + prefilter vs flat `compute`, not prefilter-on vs
  prefilter-off.** Part of the win (most visibly at 240x80, 43 → 10 us) is the
  tile path itself, not solely the row prefilter. A clean isolation (same tile
  path, prefilter toggled) needs a config switch that this bead did not add to
  avoid churn across ~12 `TileDiffConfig` literals; that A/B, and the keep/remove
  verdict, belong to `g28-decision` / `g28-impl`. The direct, deterministic
  evidence that the SAT now does real work is the `skipped_tile_rows` counter:
  2, 5 and 7 tile rows retired that the old code would have walked per buffer
  row.
- The existing `diff/sparse_5pct` group benchmarks `BufferDiff::compute` (the
  flat path), which this change does not touch, so a before/after there is ~0 by
  construction; the meaningful measurement is the new concentrated-rows group.
- A concentrated frame trips the dense-tile fallback quickly: at 120x40 with
  default ratios, 3 of 5 tile rows dirty is 60% of tiles, which hits
  `dense_tile_ratio = 0.60` and bails to the flat path before the prefilter runs.
  That is a real property of the current thresholds, noted here for
  `g28-decision`'s threshold experiment.
- Timing was collected on a shared remote worker; wall-clock is noisy. The
  correctness of the prefilter (identical output to the flat diff) is proven by
  `diff::tests::sat_prefilter_props::diff_output_identical_with_row_prefilter`
  (1000 random sparse pairs) and the naive-sum property tests, which do not
  depend on timing.
