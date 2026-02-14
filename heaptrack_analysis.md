# Heaptrack Analysis: FrankenTUI Demo Render Loop (bd-3jlw5.8)

Generated: 2026-02-14
Binary: `profile_sweep` (43 screens x 2 sizes x 20 cycles = 1,720 renders)

## Summary

| Metric | Value |
|--------|-------|
| Total allocations | 1,934,279 |
| Leaked allocations | 293 |
| Temporary allocations | 128,866 (6.7%) |
| Allocations per frame | ~1,125 |
| Peak memory | ~9.05 MB |

## Top-5 Allocation-Heavy Call Sites

| Rank | Call Site | Alloc Count | Peak Memory | Category |
|------|-----------|-------------|-------------|----------|
| 1 | `RawVecInner::finish_grow` (Vec reallocation) | 1,119,231 | 9.05 MB | Vec growth |
| 2 | `RawVecInner::try_allocate_in` (new Vec) | 471,085 | 3.16 MB | Vec creation |
| 3 | `alloc::alloc` (Box/other heap) | 262,709 | 3.17 MB | Box/heap |
| 4 | `alloc::realloc` (resize) | 70,930 | 958 KB | Reallocation |
| 5 | `Global::alloc_impl` (allocator) | 6,134 | 15 KB | Misc alloc |

## Dominant Allocation Chains

### Chain 1: Markdown LaTeX Rendering (largest by count)
```
unicodeit::naive_replace::replace  (506,080 temporary allocs)
  ← str::replace
  ← unicodeit::replace
  ← ftui_extras::markdown::latex_to_unicode
  ← MarkdownRenderer::render
  ← MarkdownRichText::render_markdown_panel
  ← Screen::view
```
**Impact**: 506K temporary allocations (26% of total) from naive substring
replacement in LaTeX-to-Unicode conversion. Each call to `str::replace`
allocates a new String.

### Chain 2: Markdown Regex/PNG parsing (second largest)
```
regex_syntax extend_from_slice (337,320 allocs)
  ← str::replace
  ← unicodeit → markdown pipeline
```

### Chain 3: Buffer/Frame allocation
```
Frame::new → Buffer cell allocation
  ← AppModel::view (per render)
```
**Impact**: New Frame + Buffer allocated per render cycle in the profiling
binary. In production, buffers are reused across frames (double-buffering),
so this overstates production impact.

### Chain 4: SmallVec/String growth in widget rendering
```
String/Vec growth in text layout, style application, line wrapping
```

## Memory Stability

- Peak memory: 9.05 MB (modest for 43 screens)
- Leaked: 293 allocations (likely lazy static/OnceCell initializations)
- No unbounded growth detected

## Allocation Rate Analysis

- ~1,125 allocs/frame average
- **Markdown screen dominates**: With markdown rendering disabled, allocation
  count would drop by ~50%
- Frame buffer allocation (in profiling binary): ~2 allocs/frame
  (negligible in production with buffer reuse)

## Optimization Targets

1. **`unicodeit::naive_replace`** (506K allocs): Cache LaTeX-to-Unicode
   results or use an allocation-free replacement strategy. This single
   function accounts for 26% of all allocations.

2. **Vec reallocation** (1.1M calls): Pre-size vectors with known capacities
   for text layout buffers, style runs, and span collectors.

3. **Per-frame String allocation**: Reuse scratch buffers across frames
   for text formatting, line wrapping, and style computation.

4. **Arena allocation** (bd-2alzw): A per-frame arena would eliminate
   individual malloc/free calls for short-lived allocations.

5. **Regex compilation caching**: If regex patterns are recompiled per frame,
   caching compiled patterns would reduce allocations.

## Artifacts

- `heaptrack.profile_sweep.*.zst`: Raw heaptrack data
- Analyze interactively: `heaptrack --analyze <file>`
