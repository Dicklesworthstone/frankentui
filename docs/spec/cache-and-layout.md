# Cache and Layout Rationale

> Why 16-byte cells, row-major storage, and sequential scan drive ftui's performance.

---

## The 16-Byte Cell

Every cell in the terminal grid occupies exactly **16 bytes**:

```text
Cell (16 bytes, #[repr(C, align(16))]) {
    content: CellContent,  // 4 bytes - char or GraphemeId
    fg: PackedRgba,        // 4 bytes - foreground color (RGBA packed u32)
    bg: PackedRgba,        // 4 bytes - background color (RGBA packed u32)
    attrs: CellAttrs,      // 4 bytes - style flags + link ID
}
```

Source: [`crates/ftui-render/src/cell.rs`](../../crates/ftui-render/src/cell.rs)

### Why 16 bytes?

A standard x86-64 cache line is 64 bytes. At 16 bytes per cell, exactly **4 cells fit
in one cache line**. This is the design's central constraint: every access pattern is
optimized around groups of 4 adjacent cells.

| Cell size | Cells per cache line | Waste per line | Verdict |
|-----------|---------------------|----------------|---------|
| 8 bytes   | 8                   | 0              | Not enough data per cell (no colors) |
| 12 bytes  | 5 (with 4B waste)   | 4 bytes (6%)   | Misaligned, wastes cache |
| **16 bytes** | **4**            | **0**          | **Perfect fit, SIMD-friendly** |
| 24 bytes  | 2 (with 16B waste)  | 16 bytes (25%) | Significant waste |
| 32 bytes  | 2                   | 0              | Doubles memory bandwidth |

The 16-byte size also aligns with 128-bit SIMD registers (SSE2/NEON), enabling
branchless cell comparison via a single vector operation.

### What fits in 16 bytes

The budget is tight but sufficient:

- **CellContent (4 bytes):** Stores either a Unicode scalar value (21 bits) or a
  `GraphemeId` reference into the grapheme pool. Bit 31 discriminates the two cases.
  A `GraphemeId` packs a 24-bit pool slot index and a 7-bit display width into 31 bits.

- **PackedRgba (4 bytes each, fg + bg = 8 bytes):** Full RGBA color packed as `u32`.
  Channel extraction and Porter-Duff alpha compositing (`over()`) operate directly on
  the packed representation without unpacking to floats. Alpha of 0 means "default
  terminal color" (not transparent-to-black).

- **CellAttrs (4 bytes):** 16-bit bitflags for style attributes (bold, italic,
  underline, etc.) plus a 16-bit link ID for hyperlink tracking.

### What does NOT fit

No per-cell heap allocation. Complex grapheme clusters (ZWJ sequences, emoji with
skin tones) are stored in the `GraphemePool` and referenced by 24-bit slot ID.
This keeps 99%+ of cells (ASCII, common Unicode) as flat value types.

---

## Row-Major Storage

```rust
// Buffer stores cells in row-major order:
// index = y * width + x
cells: Vec<Cell>
```

Source: [`crates/ftui-render/src/buffer.rs`](../../crates/ftui-render/src/buffer.rs)

Row-major storage means that cells on the same row are contiguous in memory.
Terminal rendering is inherently row-oriented: the cursor moves left-to-right,
top-to-bottom. Row-major layout aligns memory layout with access pattern.

### Consequences for the diff algorithm

The diff between two buffers (`BufferDiff::compute`) scans in row-major order:

```
for y in 0..height {
    for x in 0..width {
        if old[y][x] != new[y][x] { record change }
    }
}
```

Source: [`crates/ftui-render/src/diff.rs`](../../crates/ftui-render/src/diff.rs)

This sequential scan has two cache-friendly properties:

1. **Prefetcher utilization:** Modern CPUs detect sequential access patterns and
   prefetch the next cache line before it's needed. With 4 cells per line, the
   prefetcher stays well ahead of the scan.

2. **Row-skip fast path:** Before scanning individual cells in a row, the diff
   compares entire row slices (`old_row == new_row`). For a typical UI update
   where 95% of rows are unchanged, this skips the vast majority of work. The
   slice comparison itself benefits from auto-vectorization over aligned 16-byte
   cells.

### Run coalescing

After computing changed positions, `BufferDiff::runs()` groups adjacent changes
on the same row into `ChangeRun { y, x0, x1 }` ranges. The presenter can then
emit one cursor-position command per run rather than per cell, reducing ANSI output
volume.

---

## Comparison: `bits_eq` vs `PartialEq`

The `Cell` type provides two comparison methods:

| Method | Mechanism | Branch prediction | Use case |
|--------|-----------|-------------------|----------|
| `bits_eq(&self, other: &Cell) -> bool` | Bitwise AND of all 4 `u32` fields | Branchless | Diff inner loop |
| `PartialEq` (derived) | Field-by-field comparison | Short-circuit | General equality |

`bits_eq` is designed for the diff hot path where branch misprediction is costly.
It compares all 128 bits without early exit:

```rust
fn bits_eq(&self, other: &Cell) -> bool {
    self.content.0 == other.content.0
        & (self.fg.0 == other.fg.0)
        & (self.bg.0 == other.bg.0)
        & (self.attrs.0 == other.attrs.0)
}
```

The `&` (bitwise AND) instead of `&&` (short-circuit AND) prevents branch
misprediction when cells differ in unpredictable positions.

---

## Performance Budgets

These budgets are validated by the benchmark suite.

| Operation | Budget | Benchmark |
|-----------|--------|-----------|
| Cell comparison (`bits_eq`) | < 1 ns | `cell_bench::cell/compare/bits_eq_*` |
| Row comparison (80 cells) | < 100 ns | `cell_bench::cell/row_compare/80_cells_*` |
| Buffer diff (80x24, ~5% changed) | < 10 us | `diff_bench` |
| Present (80x24, ~5% changed) | < 1.0 ms (p50) | `presenter_bench` |
| Buffer allocation (80x24) | O(n) cells | `buffer_bench::buffer/new/alloc` |
| Buffer fill | O(n) cells | `buffer_bench::buffer/fill/fill_all` |

Benchmark sources:
- [`crates/ftui-render/benches/cell_bench.rs`](../../crates/ftui-render/benches/cell_bench.rs)
- [`crates/ftui-render/benches/buffer_bench.rs`](../../crates/ftui-render/benches/buffer_bench.rs)
- [`crates/ftui-render/benches/diff_bench.rs`](../../crates/ftui-render/benches/diff_bench.rs)
- [`crates/ftui-render/benches/presenter_bench.rs`](../../crates/ftui-render/benches/presenter_bench.rs)

Run benchmarks with:
```bash
cargo bench -p ftui-render
```

---

## Must-Hold Invariants vs Expected Performance

### Must-hold (enforced by compile-time or runtime assertions)

| Invariant | Enforcement |
|-----------|-------------|
| `size_of::<Cell>() == 16` | Static assert in `cell.rs` |
| `align_of::<Cell>() == 16` | `#[repr(C, align(16))]` |
| `size_of::<CellContent>() == 4` | Static assert |
| `size_of::<PackedRgba>() == 4` | Static assert |
| `size_of::<CellAttrs>() == 4` | Static assert |
| Buffer dimensions immutable after creation | No `resize` API |
| `cells.len() == width * height` | Checked in `Buffer::new` |

### Expected performance (validated by benchmarks, not guaranteed)

| Expectation | Basis |
|-------------|-------|
| 4 cells per cache line | Depends on 64-byte cache lines (x86-64, ARM Cortex-A) |
| SIMD comparison of cells | Requires compiler auto-vectorization or explicit SIMD |
| Prefetcher effectiveness | Depends on sequential access, CPU microarchitecture |
| Row-skip eliminates 95% of diff work | Depends on typical UI update patterns |

The distinction matters: changing `Cell` size would break the build (static assert),
but running on a CPU with 32-byte cache lines would only degrade performance, not
correctness.

---

## Design Alternatives Considered

### Per-cell String storage (rejected)

Storing grapheme strings inline (e.g., `SmallString<8>`) would increase cell size
to 24+ bytes, wasting 25% of each cache line and breaking SIMD alignment. The
`GraphemePool` indirection costs one lookup for the rare complex-grapheme case
while keeping the common case (single `char`) zero-cost.

### Column-major storage (rejected)

Column-major layout would optimize for vertical scrolling but penalize the vastly
more common horizontal scan used in diffing and rendering. Terminal output is
inherently row-oriented.

### Separate style buffer (rejected)

Storing styles in a parallel `Vec<Style>` would halve the cell size but double the
number of cache lines accessed during diff and render. The unified layout keeps all
per-cell data in one cache line access.
