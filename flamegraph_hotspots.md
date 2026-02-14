# Flamegraph Analysis: FrankenTUI Demo Render Loop (bd-3jlw5.7)

Generated: 2026-02-14
Binary: `profile_sweep` (43 screens x 2 sizes x 100 cycles = 8,600 renders)
Throughput: ~3,076 renders/sec (release profile)

## Top-5 CPU Hotspots

| Rank | Symbol | % Time | Samples | Location |
|------|--------|--------|---------|----------|
| 1 | `Buffer::mark_dirty_span` | 10.10% | 283 | `ftui-render/src/buffer.rs` |
| 2 | `StrSearcher::new` (std) | 4.15% | 116 | core::str::pattern (stdlib) |
| 3 | `Graphemes::next` (unicode-segmentation) | 3.17% | 94 | unicode_segmentation::grapheme |
| 4 | `Buffer::get_mut` | 3.09% | 85 | `ftui-render/src/buffer.rs` |
| 5 | `set_style_area` | 2.74% | 76 | `ftui-widgets/src/lib.rs` |

## Full Hotspot Breakdown (>1%)

| Symbol | % Time | Category |
|--------|--------|----------|
| `Buffer::mark_dirty_span` | 10.10% | Dirty tracking |
| `StrSearcher::new` | 4.15% | String search |
| `Graphemes::next` | 3.17% | Unicode grapheme iteration |
| `Buffer::get_mut` | 3.09% | Cell access |
| `set_style_area` | 2.74% | Style application |
| `Buffer::set_fast` | 2.43% | Cell mutation |
| `InCB_Extend` (unicode tables) | 2.41% | Unicode property lookup |
| `CellContent::width` | 2.29% | Width calculation |
| `Buffer::index` | 2.06% | Array indexing |
| `Buffer::mark_dirty_bits_range` | 1.95% | Dirty bit tracking |
| `MetaballsCanvasAdapter::fill` | 1.85% | VFX rendering |
| `gradient_color` | 1.63% | VFX color computation |
| `memmove_avx` (libc) | 1.57% | Memory copy |
| `draw_text_span_scrolled` | 1.54% | Text rendering |
| `TwoWaySearcher::next` (std) | 1.53% | String matching |
| `ascii_width` iterator | 1.47% | Width checking |
| `unicode_display_width` | 1.43% | Unicode width |
| `Shakespeare::toc_entries` (OnceCell) | 1.41% | Lazy init |
| `Buffer::set` | 1.32% | Cell mutation |
| `_int_malloc` (libc) | 1.27% | Heap allocation |
| `count_breaks` (str_indices) | 1.21% | Line counting |
| `next_code_point` (std) | 1.21% | UTF-8 decoding |
| `Vec<Cell>::index` | 1.13% | Array bounds check |
| `PackedRgba::over` | 1.07% | Color compositing |
| `RawVecInner::finish_grow` | 1.06% | Vec reallocation |

## Key Findings

1. **Dirty tracking dominates** (12.05%): `mark_dirty_span` + `mark_dirty_bits_range`
   account for the largest CPU time. Optimizing the dirty tracking algorithm
   (e.g., batching span merges, reducing per-cell overhead) would have the
   highest impact.

2. **Buffer cell access** (8.90%): `get_mut` + `set_fast` + `set` + `index` +
   `Vec<Cell>::index` collectively represent significant overhead from bounds
   checking and dirty-span bookkeeping on every cell mutation.

3. **Unicode processing** (10.48%): Grapheme segmentation, width calculation,
   unicode property lookups, and UTF-8 decoding. This is an inherent cost
   but caching (e.g., S3-FIFO width cache) could reduce repeated lookups.

4. **String operations** (5.68%): `StrSearcher::new` + `TwoWaySearcher::next`
   suggest repeated substring searches (likely in text rendering or markup).

5. **Allocation** (2.33%): `_int_malloc` + `RawVecInner::finish_grow` show
   heap allocation pressure. Arena allocation could mitigate this.

## Optimization Recommendations

1. **Reduce dirty-span overhead**: Consider a simpler dirty-row bitmap
   without per-row span tracking for the render path, deferring span
   computation to the diff phase.

2. **Batch cell mutations**: The `set_style_area` function calls per-cell
   dirty tracking; a batch-aware path could amortize the overhead.

3. **Cache unicode widths**: The S3-FIFO cache (bd-l6yba) would directly
   help with `CellContent::width` and grapheme iteration costs.

4. **Arena allocation**: Pre-allocate per-frame scratch buffers to reduce
   malloc/free churn (addresses `_int_malloc` + `finish_grow`).

## Artifacts

- `flamegraph.svg`: Interactive flame graph (open in browser)
- `perf.data`: Raw perf recording for further analysis
