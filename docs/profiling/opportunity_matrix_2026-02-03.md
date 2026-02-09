# Baseline Profiling + Opportunity Matrix (bd-1rz0.13)

**Date:** 2026-02-03
**Agent:** PinkOtter
**Tools:** cargo bench (criterion), hyperfine

---

## Executive Summary

Baseline benchmarks captured for ftui-render (buffer, diff, cell) and ftui-layout (flex, grid) at key terminal sizes (80x24, 120x40, 200x60). Overall performance is excellent with most operations completing in microseconds or nanoseconds.

---

## Baseline Metrics Summary

### Buffer Operations (ftui-render)

| Operation | 80x24 | 120x40 | 200x60 | Notes |
|-----------|-------|--------|--------|-------|
| **Alloc** | 549ns | 1.26µs | 3.13µs | ~3.5-3.8 Gelem/s |
| **Clone** | 528ns | 1.29µs | 3.33µs | ~3.6 Gelem/s |
| **Fill (full)** | 31.7µs | 82.3µs | 206µs | ~58-60 Melem/s |
| **Clear** | 477ns | - | 3.12µs | ~4.0 Gelem/s |
| **Set (single)** | 16.2ns | - | - | 4x overhead vs set_raw |
| **Set (row 80)** | 1.28µs | - | - | ~16ns/cell |

### Diff Operations (ftui-render)

| Scenario | 80x24 | 120x40 | 200x60 | Notes |
|----------|-------|--------|--------|-------|
| **Identical (0%)** | 1.81µs | 4.45µs | 11.2µs | ~1.06 Gelem/s |
| **Sparse (5%)** | 2.56µs | 5.89µs | 12.6µs | ~750-955 Melem/s |
| **Heavy (50%)** | 3.43µs | 6.47µs | 13.4µs | ~560-894 Melem/s |
| **Full (100%)** | 2.29µs | 5.99µs | 14.0µs | ~802-856 Melem/s |

### Cell Operations (ftui-render)

| Operation | Time | Notes |
|-----------|------|-------|
| **bits_eq** | 2.2ns | Fast equality check |
| **from_char (ASCII)** | 0.98ns | Sub-nanosecond |
| **from_char (CJK)** | 0.92ns | No penalty for wide chars |
| **from_char (styled)** | 1.01ns | Minimal styling overhead |
| **PackedRgba::over** | 2.52ns | Alpha blending |

### Layout Operations (ftui-layout)

| Operation | Time | Notes |
|-----------|------|-------|
| **Flex 3 constraints** | 84ns | Fast for typical UI |
| **Flex 10 constraints** | 200ns | Scales linearly |
| **Flex 50 constraints** | 448ns | Sub-microsecond |
| **Grid 3x3** | 152ns | Acceptable |
| **Grid 10x10** | 491ns | Under 500µs budget |
| **Grid 20x20** | 847ns | Still under 1µs |
| **Nested 3col x 10row** | 400ns | Real-world scenario |

---

## Opportunity Matrix

Scored by: **Impact × Confidence / Effort** (Score ≥ 2.0 = implement)

| ID | Opportunity | Impact | Confidence | Effort | Score | Recommendation |
|----|-------------|--------|------------|--------|-------|----------------|
| O1 | **Buffer::fill optimization** | 8 | 7 | 5 | **11.2** | SIMD memset for Cell arrays |
| O2 | **Set vs set_raw gap** | 6 | 9 | 3 | **18.0** | Inline scissor check; fast path |
| O3 | **Diff dirty-row skip** | 9 | 8 | 7 | **10.3** | Track dirty rows to skip comparisons |
| O4 | **Cell::bits_eq SIMD** | 5 | 6 | 6 | **5.0** | Already 2.2ns; diminishing returns |
| O5 | **Layout constraint caching** | 6 | 7 | 4 | **10.5** | Memoize unchanged layouts |
| O6 | **Grid row-major optimization** | 4 | 6 | 5 | **4.8** | Below threshold |

### Top 3 Recommendations

1. **O2: Set vs set_raw gap (Score 18.0)**
   - `set_single` is 16.2ns vs `set_raw_single` at 4.07ns (4x overhead)
   - Scissor/opacity check can be inlined and fast-pathed
   - Low effort, high confidence, measurable impact

2. **O1: Buffer::fill SIMD (Score 11.2)**
   - Fill at ~58 Melem/s for large buffers
   - SIMD can push to 200+ Melem/s with AVX2
   - Moderate effort (portable_simd integration)

3. **O5: Layout constraint caching (Score 10.5)**
   - Layouts often unchanged between frames
   - Memoization can skip redundant computation
   - Ties into bd-4kq0.4 (temporal coherence)

---

## VFX Hotspot Matrix (Template)

Use this template for Visual Effects profiling passes (bd-3e1t.5.x).

### Capture Inputs

- Screen/Effect: `VisualEffects::<name>`
- Mode/Size: `alt 120x40` (or `inline 80x24`)
- Seed: `FTUI_DEMO_SEED=<n>`
- Scenario: `steady` | `burst` | `resize` | `startup`

### Metrics (per run)

- `init_ms` (time to first frame)
- `frame_ms_p50`, `frame_ms_p95`, `frame_ms_p99`
- `allocs_per_frame` (if available)
- `hash_stability` (determinism check)

### Hotspot Table

| Screen/Effect | Scenario | Size/Mode | Top Hotspots | Evidence | Hypothesis | Candidate Fix | Expected Gain | Risk |
|---|---|---|---|---|---|---|---|---|
| Doom/Quake | startup | 120x40 alt | `pick_spawn`, `wall_distance` | flamegraph | expensive full scan | bounded scan + cache | 2-4x startup | low |

### Scoring (same formula)

Score each candidate with **Impact × Confidence / Effort**. Track at least one
“no‑code” idea (cache key, precompute, or lazy init) to keep risk low.

### VFX Pass: 2026-02-09 (bd-3e1t.5.3)

This pass re-ran deterministic PTY harness measurements for the heavy effects
(`plasma`, `metaballs`) at `120x40` and `200x60` using:

- `--vfx-harness --vfx-tick-ms=16 --vfx-frames=180 --vfx-perf --vfx-seed=12345`
- crossterm-compat build for PTY compatibility
- JSONL artifacts under `.scratch/vfx/`

#### Hotspot / Opportunity Matrix (updated)

| ID | Hotspot | Impact | Confidence | Effort | Score | Status |
|----|---------|--------|------------|--------|-------|--------|
| H1 | `Painter::braille_cell` per-subpixel bounds/index checks | 5 | 4 | 2 | **10.0** | Implemented (fast in-bounds path) |
| H2 | `Painter::clear` full-buffer reset each frame | 4 | 4 | 2 | **8.0** | Implemented (generation-stamp O(1) clear) |
| H3 | `MetaballsCanvasAdapter::fill` field accumulation loops | 5 | 3 | 3 | **5.0** | In progress (`bd-3e1t.5.3.1`, MistyCliff) |
| H4 | `PlasmaCanvasAdapter::fill` per-pixel palette interpolation | 4 | 3 | 3 | **4.0** | Implemented (`bd-3e1t.5.9`) |
| H5 | Presenter ANSI emission on high-churn frames | 3 | 3 | 3 | **3.0** | Pending |
| H6 | `Painter::point_colored_at_index_in_bounds` branch-elision trial | 4 | 3 | 2 | **6.0** | Trial reverted (regressed plasma/metaballs) |

#### Measured deltas

Baseline files:
- `.scratch/vfx/bd-3e1t.5.3_plasma_120x40_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_plasma_200x60_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_metaballs_120x40_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_metaballs_200x60_crossterm.jsonl`

After `braille_cell` fast path:
- `.scratch/vfx/bd-3e1t.5.3_post_canvas_plasma_120x40_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_post_canvas_plasma_200x60_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_post_canvas_metaballs_120x40_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_post_canvas_metaballs_200x60_crossterm.jsonl`

After `Painter` generation-stamp clear:
- `.scratch/vfx/bd-3e1t.5.3_post_gen_plasma_120x40_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_post_gen_plasma_200x60_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_post_gen_metaballs_120x40_crossterm.jsonl`
- `.scratch/vfx/bd-3e1t.5.3_post_gen_metaballs_200x60_crossterm.jsonl`

`total_ms_p95` (base -> post_canvas -> post_gen):

| Effect/Size | Base | Post Canvas | Post Gen | Net vs Base |
|---|---:|---:|---:|---:|
| plasma 120x40 | 3.461 | 2.989 | 3.166 | -8.52% |
| plasma 200x60 | 6.821 | 6.677 | 6.692 | -1.89% |
| metaballs 120x40 | 3.436 | 3.467 | 3.341 | -2.76% |
| metaballs 200x60 | 7.256 | 6.823 | 7.226 | -0.41% |

`render_ms_p95` (base -> post_canvas -> post_gen):

| Effect/Size | Base | Post Canvas | Post Gen | Net vs Base |
|---|---:|---:|---:|---:|
| plasma 120x40 | 2.521 | 2.072 | 2.111 | -16.26% |
| plasma 200x60 | 4.651 | 4.552 | 4.806 | +3.33% |
| metaballs 120x40 | 2.574 | 2.500 | 2.249 | -12.63% |
| metaballs 200x60 | 5.453 | 5.034 | 5.157 | -5.43% |

Additional candidate heavy effects (base -> post_gen at 120x40):

| Effect/Size | total_ms_p95 base | total_ms_p95 post_gen | Delta |
|---|---:|---:|---:|
| doom 120x40 | 1.386 | 1.262 | -8.95% |
| quake 120x40 | 2.661 | 2.485 | -6.61% |

#### Follow-up sweep: current HEAD + reverted `canvas.rs` trial (2026-02-09)

This follow-up used the same deterministic harness shape:

- `--vfx-harness --vfx-cols=120 --vfx-rows=40 --vfx-tick-ms=16 --vfx-frames=180 --vfx-seed=12345 --vfx-perf`
- Baseline inputs: `.scratch/vfx/bd-3e1t.5.3_{plasma,metaballs,doom,quake}_120x40_crossterm.jsonl`
- Current inputs: `/tmp/vfx_bd3e1t53_current_{plasma,metaballs,doom,quake}_120x40.jsonl`
- Reverted trial inputs: `/tmp/vfx_bd3e1t53_trial_canvasbranchless_{plasma,metaballs,doom,quake}_120x40.jsonl`

`total_ms_p95` / `render_ms_p95` (base -> current):

| Effect | Base total | Current total | Delta | Base render | Current render | Delta |
|---|---:|---:|---:|---:|---:|---:|
| plasma | 3.461 | 2.587 | -25.25% | 2.521 | 1.669 | -33.80% |
| metaballs | 3.436 | 3.761 | +9.46% | 2.574 | 2.951 | +14.65% |
| doom | 1.386 | 1.366 | -1.44% | 1.358 | 1.255 | -7.58% |
| quake | 2.661 | 3.030 | +13.87% | 2.634 | 2.875 | +9.15% |

Trial comparison (`canvas.rs` branch-elision, current -> trial):

| Effect | Current total | Trial total | Delta | Current render | Trial render | Delta |
|---|---:|---:|---:|---:|---:|---:|
| plasma | 2.587 | 2.947 | +13.92% | 1.669 | 1.957 | +17.26% |
| metaballs | 3.761 | 3.941 | +4.79% | 2.951 | 2.969 | +0.61% |
| doom | 1.366 | 1.263 | -7.54% | 1.255 | 1.177 | -6.22% |
| quake | 3.030 | 2.771 | -8.55% | 2.875 | 2.662 | -7.41% |

Decision: revert branch-elision lever in `canvas.rs` because it regresses the two target-heavy effects (`plasma`, `metaballs`) despite improving `doom`/`quake`.

#### Presenter hot-path trial (`bd-3e1t.5.3.3`) — reverted

Surface explored: `crates/ftui-render/src/presenter.rs` (`emit_style_delta` color-only fast path).

Two variants were tested against current head and both were reverted (no net source change retained):

- V1 artifacts: `/tmp/vfx_bd3e1t53_post_presenterfast_{plasma,metaballs,doom,quake}_120x40.jsonl`
- V2 artifacts: `/tmp/vfx_bd3e1t53_post_presenterfastv2_{plasma,metaballs}_120x40.jsonl`

V1 (`current -> presenterfast`, p95 deltas):

| Effect | total_ms_p95 | render_ms_p95 | present_ms_p95 |
|---|---:|---:|---:|
| plasma | +19.64% | +12.40% | +15.12% |
| metaballs | +9.25% | -5.90% | +76.75% |
| doom | -9.59% | -6.53% | -3.11% |
| quake | -17.39% | -16.28% | +4.55% |

V2 (`current -> presenterfastv2`, heavy effects):

| Effect | total_ms_p95 | render_ms_p95 | present_ms_p95 |
|---|---:|---:|---:|
| plasma | +23.61% | +12.52% | +53.73% |
| metaballs | +2.66% | +0.17% | +15.03% |

Determinism remained identical for all runs (frame-hash stream SHA256 unchanged per effect), but heavy-effect performance regressed, so this lever is marked as a dead end for the current pass.

#### Detached-worktree A/B (bd-3e1t.5.9 plasma commit pair)

- Baseline commit: `0fcc6ce5ebdd7524a408b6f20b104dd4a97377ed`
- Optimized commit: `295e8c77da6977157c124ff9a5183cd471f7073b`

| Run ID | Effect | Frames | total_ms_p95 | render_ms_p95 | present_ms_p95 |
|---|---|---:|---:|---:|---:|
| `plasma-before-295e8c77` | plasma | 401 | 2.649 | 1.747 | 0.977 |
| `plasma-after-295e8c77` | plasma | 401 | 2.693 | 1.791 | 1.080 |

Determinism checks:

| Sweep | SHA256(frame-hash stream) |
|---|---|
| plasma A/B pair (`before`/`after`) | `6fb20e469d4f064a81c695bb5b570246a365c80c3fcc03846dfdd16765c3b3b7` |
| plasma base/current/trial | `f6614d778157c8df8c7cbd2013287903dfbb5e6b9df8e23998e7e51290792109` |
| metaballs base/current/trial | `da23e973ab727f363e37441630baf5e3994dc7ce7e8f11b685f2483cfa7a0c48` |
| doom base/current/trial | `2d6ce9188c7940b16ef846395d5618d6bb0cea490eccef11a876a8232ca74636` |
| quake base/current/trial | `f2ac352209d03c920e2ce47443f6b08bfb91229785df82cd1096f151d57605e1` |

#### Isomorphism notes

- `braille_cell` fast path is algorithmically equivalent to the slow path:
  identical dot-bit mapping and "first lit pixel color wins" ordering.
- Generation-based clear preserves frame semantics:
  a pixel is visible iff written in the current generation; uncolored writes
  explicitly clear stale color at write-site (`point` sets `None`).

---

## Hotspots Identified

1. **Buffer::fill** - Largest time consumer for full-screen operations
2. **Buffer::set overhead** - 4x slower than set_raw due to checks
3. **Diff at 50% change** - Slightly slower than 0% or 100% (mixed workload)

---

## Frame Budget Analysis

**Target:** 16.67ms (60 FPS) or 8.33ms (120 FPS)

| Component | 80x24 | 120x40 | 200x60 | % of 60fps |
|-----------|-------|--------|--------|------------|
| Buffer alloc | 0.5µs | 1.3µs | 3.1µs | 0.02% |
| Diff (5%) | 2.6µs | 5.9µs | 12.6µs | 0.08% |
| Fill (full) | 31.7µs | 82.3µs | 206µs | 1.2% |
| Layout (nested) | 0.4µs | - | - | 0.002% |
| **Total baseline** | ~35µs | ~90µs | ~222µs | **1.3%** |

**Conclusion:** Render kernel uses <2% of frame budget. Plenty of headroom for degradation-based quality tiers.

---

## Next Steps

1. **Profile presenter** - ANSI emission not yet benchmarked (run presenter_bench)
2. **Profile text** - Width calculation not yet included (run width_bench)
3. **CPU flamegraph** - Identify call-graph hotspots in realistic workloads
4. **Allocation profiling** - Track heap allocations per frame

---

## Artifacts

- `docs/profiling/baseline_metrics_2026-02-03.jsonl` - Raw metrics in JSONL
- `docs/profiling/diff_bench_baseline.txt` - Full diff bench output
- `docs/profiling/layout_bench_baseline.txt` - Full layout bench output
- `docs/profiling/buffer_bench_baseline.txt` - Full buffer bench output

---

## Reproducibility

```bash
# Re-run benchmarks
cargo bench -p ftui-render --bench diff_bench
cargo bench -p ftui-render --bench buffer_bench
cargo bench -p ftui-layout --bench layout_bench

# Compare with baseline
cargo bench -p ftui-render --bench diff_bench -- --baseline baseline_2026-02-03
```

---

## VOI Sampling Policy (bd-1rz0.28)

**Baseline (hyperfine):** `cargo test -p ftui-runtime perf_voi_sampling_budget -- --nocapture`

- p50: 166.456ms
- p95: 172.368ms
- p99: 246.832ms

**Flamegraph:** captured 2026-02-03 at `docs/profiling/bd-1rz0.28/voi_sampling_flamegraph.svg` (release profile, no debuginfo).

### Opportunity Matrix (Pending Analysis)
Flamegraph captured; hotspots still need to be summarized.

| ID | Opportunity | Impact | Confidence | Effort | Score | Recommendation |
|----|-------------|--------|------------|--------|-------|----------------|
| O1 | Reduce decision struct cloning | 3 | 3 | 2 | 4.5 | Re-evaluate after flamegraph |
| O2 | Inline VOI math helpers | 2 | 2 | 1 | 4.0 | Re-evaluate after flamegraph |

### Notes
- Flamegraph command: `cargo flamegraph -p ftui-runtime --unit-test -o docs/profiling/bd-1rz0.28/voi_sampling_flamegraph.svg -- perf_voi_sampling_budget --nocapture`
- Perf access was temporarily enabled via `kernel.perf_event_paranoid=1` and then restored to `4`.
- To improve symbolization, rerun with debuginfo: set `CARGO_PROFILE_RELEASE_DEBUG=true` or add `[profile.release] debug = true`.
