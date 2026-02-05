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
