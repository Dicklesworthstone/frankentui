# Baseline Profiles & Hotspot Opportunity Matrix

> bd-2vr05.15.4.1 — Collected 2026-02-16 on Contabo VPS workers (rch fleet)

## 1. Pipeline Baselines

### 1.1 Text Shaping Pipeline (ftui-text)

| Operation | Input | Latency | Throughput | Notes |
|-----------|-------|---------|------------|-------|
| `ClusterMap::from_text` | latin/10K | 480µs | 19.8 MiB/s | Grapheme iteration + entry allocation |
| `ClusterMap::from_text` | cjk/10K | 343µs | 27.8 MiB/s | Fewer graphemes per byte |
| `ClusterMap::from_text` | latin/100K | 3.85ms | 24.7 MiB/s | Linear scaling confirmed |
| `ClusterMap::byte_to_cell` | 270 lookups/10K | 7µs | — | Binary search, O(log n) per lookup |
| `ClusterMap::cell_to_byte` | 270 lookups/10K | 8.4µs | — | Reverse binary search |
| `ClusterMap::cell_range_to_byte_range` | 200 lookups/10K | 17.6µs | — | Two binary searches per call |
| `ClusterMap::extract_text` | small ranges | 15.7µs | — | Multiple small extractions |
| `ShapedLineLayout::from_text` | latin/10K | 609µs | 15.6 MiB/s | Creates ClusterMap + placements |
| `ShapedLineLayout::from_text` | cjk/10K | 462µs | 20.6 MiB/s | |
| `ShapedLineLayout::from_run` | latin/10K | **53ms** | 183 KiB/s | **O(n^2) — CRITICAL HOTSPOT** |
| `ShapedLineLayout::from_run` | cjk/10K | 7.2ms | 1.3 MiB/s | 7x faster (fewer glyphs/byte) |
| `apply_justification` | 10K | 83µs | 115 MiB/s | Linear scan + spacing adjustment |
| `apply_tracking` | 10K | 47µs | 201 MiB/s | Simple per-placement addition |
| `placement_at_cell` | 270 lookups/10K | 905µs | — | Linear scan (not indexed) |

### 1.2 Width Calculation (ftui-text)

| Operation | Input | Latency | Throughput | Notes |
|-----------|-------|---------|------------|-------|
| `width` (ascii) | 1K chars | 6.5µs | 140 MiB/s | Fast path |
| `width` (cjk) | 1K chars | 33µs | 27 MiB/s | Unicode width lookup per char |
| `width` (emoji) | 1K chars | 56µs | 68 MiB/s | ZWJ/combining complexity |
| `segment_width` (ascii) | single | 65ns | — | Very fast |
| `segment_width` (cjk) | single | 6.4µs | — | |
| Cache warm hit | single | 1.4µs | — | ~2x over direct on repeated calls |

### 1.3 Width Cache (ftui-text)

| Operation | Pattern | Input | Latency | Notes |
|-----------|---------|-------|---------|-------|
| S3FIFO | zipfian | 10K | 409µs | Hot keys dominate |
| S3FIFO | scan | 10K | 1.1ms | Cold path, many misses |
| S3FIFO | mixed | 10K | 727µs | Real-world blend |
| S3FIFO | zipfian | 100K | 11.8ms | Linear scaling |
| S3FIFO | scan | 100K | 54.7ms | Cache thrashing |

### 1.4 Layout Solver (ftui-layout)

| Operation | Input | Latency | Notes |
|-----------|-------|---------|-------|
| Flex horizontal 3-child | simple | 44ns | Near-instant |
| Flex horizontal 10-child | constraints | 152ns | Linear in children |
| Flex horizontal 50-child | constraints | 544ns | |
| Flex vertical 20-child | split | 195ns | |
| Flex vertical 50-child | split | 374ns | |
| Grid 3x3 | split | 120ns | |
| Grid 10x10 | split | 390ns | |
| Grid 20x20 | split | 669ns | |
| Nested 3col x 10row | split | 337ns | Recursion overhead minimal |

### 1.5 Render Pipeline (ftui-render)

| Operation | Input | Latency | Throughput | Notes |
|-----------|-------|---------|------------|-------|
| Cell `as_char` | single | 2.8ns | — | |
| PackedRGBA create | single | 0.9ns | — | |
| PackedRGBA `over` (partial) | single | 23.5ns | — | Alpha blending |
| Row compare (identical) | 80 cells | 155ns | — | SIMD-friendly |
| Row compare (identical) | 200 cells | 443ns | — | |
| BufferDiff compute | 240x80 sparse 5% | 28µs | 686 Melem/s | |
| BufferDiff compute | 240x80 single row | 19µs | 1.0 Gelem/s | |
| BufferDiff compute_dirty | 240x80 single row | 16µs | 1.17 Gelem/s | |
| Presenter (sparse 5%) | 80x24 | 5.3µs | 7.2 Melem/s | |
| Presenter (sparse 5%) | 200x60 | 63µs | 9.6 Melem/s | |
| Presenter (heavy 50%) | 200x60 | 113µs | 5.7 Melem/s | |
| Presenter (full 100%) | 200x60 | 370µs | 5.1 Melem/s | |
| Full pipeline (diff+present) | 80x24@5% | 18.6µs | 103 Melem/s | |
| Full pipeline (diff+present) | 200x60@5% | 71.7µs | 167 Melem/s | |
| Full pipeline (diff+present) | 200x60@50% | 89µs | 135 Melem/s | |

### 1.6 Shaping Fallback Pipeline (ftui-text)

| Operation | Input | Latency | Throughput | Notes |
|-----------|-------|---------|------------|-------|
| Terminal mode | latin/10K | 612µs | 15.5 MiB/s | from_text path |
| Terminal mode | cjk/10K | 498µs | 19.1 MiB/s | |
| Shaped NoopShaper | latin/10K | **60.8ms** | 160 KiB/s | **from_run O(n^2) dominates** |
| Shaped NoopShaper | mixed/10K | 29ms | 331 KiB/s | |
| Batch terminal | 40 lines | 209µs | 15.2 MiB/s | Per-screenful budget |

## 2. Hotspot Opportunity Matrix

Ranked by impact (latency × frequency) and optimization confidence:

| Rank | Hotspot | Current | Target | Speedup | Effort | Confidence | Blocks |
|------|---------|---------|--------|---------|--------|------------|--------|
| **1** | `ShapedLineLayout::from_run` O(n^2) | 53ms/10K | <1ms/10K | **50-100x** | Medium | High | Shaped path unusable for real text |
| **2** | `placement_at_cell` linear scan | 905µs/270 lookups | <50µs | **~18x** | Low | High | Add cell-index array or binary search |
| **3** | `ClusterMap::from_text` allocation | 480µs/10K | ~200µs | **~2x** | Medium | Medium | Pre-allocate Vec with capacity hint |
| **4** | Width cache scan pattern | 54.7ms/100K | ~12ms | **~4x** | Medium | Medium | S3FIFO eviction tuning or CLOCK-Pro |
| **5** | Presenter full-screen | 370µs/200x60 | ~200µs | **~1.8x** | High | Low | Already state-tracked; diminishing returns |
| **6** | `from_text` layout construction | 609µs/10K | ~400µs | **~1.5x** | Medium | Medium | Reduce ClusterMap + placement allocation |

### Scoring Key

- **Effort**: Low = <2h, Medium = 2-8h, High = 8h+
- **Confidence**: High = clear algorithmic fix, Medium = needs profiling, Low = near-optimal already
- **Speedup**: Estimated improvement factor

## 3. Critical Path for 60fps Budget

At 60fps, frame budget = 16.67ms. Key pipeline stages:

| Stage | Budget Share | Current (200x60) | Status |
|-------|-------------|-------------------|--------|
| Layout solve | 5% (0.8ms) | ~1µs | Well within budget |
| Text shaping (terminal) | 15% (2.5ms) | ~612µs/10K | OK for screen-sized text |
| Text shaping (shaped) | 15% (2.5ms) | **53ms** | **BLOCKS 60fps** |
| Buffer diff | 10% (1.7ms) | ~28µs | Well within budget |
| Presenter (ANSI emit) | 20% (3.3ms) | ~370µs (full) | OK |
| Headroom | 50% | — | Available for widgets, IO |

## 4. Recommendations

1. **Fix `from_run` O(n^2)** before enabling shaped rendering in production. The `sum_cluster_advance` and/or `render_hint_for_cluster` helpers likely do linear scans per glyph. Switch to pre-computed cluster boundaries.

2. **Index `placement_at_cell`** with a cell-offset lookup table for O(1) access instead of linear scan.

3. **Profile `ClusterMap::from_text`** with flame graphs to identify whether grapheme iteration or Vec allocation dominates.

4. **Terminal fallback path is production-ready** at 612µs/10K (~15 MiB/s). No optimization needed for typical terminal workloads.

5. **Layout solver is extremely fast** (<1µs for typical layouts). No optimization needed.

6. **Diff + Present pipeline** is well-optimized at ~90µs for 200x60@50% change. No immediate action.
