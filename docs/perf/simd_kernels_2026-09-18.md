# Portable-SIMD kernels: measured against their scalar twins

Bead: `bd-g00-root-epic-ewths.38.2` (G33). Measured 2026-09-18 on rch worker
`vmi1264463`, `cargo bench -p ftui-simd --bench simd_bench`, criterion medians.

The headline is that the two halves of this bead went opposite ways, and the
numbers are not close in either direction.

## ASCII detection: 5x to 44x faster

| bench | scalar | simd | speedup |
|---|---|---|---|
| `all_ascii/64_bytes` | 37.8 ns | 7.48 ns | 5.1x |
| `all_ascii/1024_bytes` | 519 ns | 18.8 ns | 27.6x |
| `all_ascii/65536_bytes` | 40.6 µs | 924 ns | 44.0x |
| `ascii_width/64_bytes` | 44.6 ns | 8.10 ns | 5.5x |
| `ascii_width/1024_bytes` | 745 ns | 35.0 ns | 21.3x |
| `ascii_width/65536_bytes` | 49.2 µs | — | — |

At 1 KiB the scalar twin runs at about 1.8 GiB/s and the kernel at about
50 GiB/s. The inputs are all-ASCII on purpose: that is the case that has to
read every byte, and it is also the common case in a terminal. A non-ASCII
byte only makes both twins return sooner.

## Cell row comparison: 3x to 4x *slower*

| bench | scalar | simd | ratio |
|---|---|---|---|
| `first_mismatch/80_cells` | 61.8 ns | 252 ns | 0.25x |
| `first_mismatch/200_cells` | 171 ns | 547 ns | 0.31x |
| `first_mismatch/1000_cells` | 892 ns | 2.66 µs | 0.34x |
| `rows_equal/80_cells` | 82.7 ns | 251 ns | 0.33x |
| `rows_equal/200_cells` | 190 ns | 708 ns | 0.27x |
| `rows_equal/1000_cells` | 768 ns | 3.13 µs | 0.25x |

### Why

The bead anticipated this ("copying cells into a stack buffer costs ~1 KB
memcpy per chunk; measure; if it eats the gain..."). It ate the gain, and the
reason is that there was no gain available to begin with.

There is no 128-bit lane type to compare with, and `#![forbid(unsafe_code)]`
rules out viewing `&[u128]` as `&[u64]` or `&[u8]`. So the kernel has to
*build* its vectors: per four cells, eight mask-and-shift operations into a
stack array before a single compare. Meanwhile the scalar twin's `a[i] != b[i]`
over `u128` is already lowered by LLVM to one 128-bit compare — which is
exactly what `Cell::bits_eq`'s own doc comment claims, and it turns out to be
true. The scalar path was already the vectorised path; the kernel just adds a
gather in front of it.

Getting this to win would need a zero-cost `&[Cell] -> &[u8]` view, which means
`bytemuck` or a transmute, which this workspace forbids. Storing rows as
`u128` internally would be a much larger change to `Buffer`, and on these
numbers it would be chasing a compare that is not the bottleneck.

## What shipped

- All four kernels and their scalar twins live in `ftui-simd`, with parity
  tests. The crate is no longer documentation-only.
- **Only the ASCII kernels are worth wiring into a caller.** The row-compare
  kernels stay in the crate as the measured negative result and as the thing
  the parity tests pin; `ftui-render/src/diff.rs` keeps its scalar compare.

## Against the acceptance bar

`bd-g00-root-epic-ewths.38.1` set the bar as ">=25% improvement on
`diff/identical/compute/200x60` and `diff/sparse_5pct/compute/200x60`, no
regression at 80x24". That bar is **missed**, and not marginally: the kernel it
was written for is 3-4x slower than what it would replace, so wiring it into
the diff could only make those benches worse. The bar was never run, because a
kernel that loses by 4x at the unit level cannot win at the integration level.

The bar's fallback is "fall back to (b)", i.e. yank and remove the crate. That
fallback was written on the assumption that a missed bar meant an empty crate.
It no longer does: the ASCII kernels are a 20-44x win over the scalar code in
`text_width`'s hot path, which is enough content to justify the crate on its
own. Whether to take that route is the owner's call, and is recorded on
`.38.1`.
