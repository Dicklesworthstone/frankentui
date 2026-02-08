# ADR-009: FrankenTerm WebGPU Renderer Architecture (Glyph Atlas, Batching, Present)

## Status

PROPOSED

## Context

We are replacing xterm.js with a first-party renderer (`frankenterm-web`) that:
- renders a terminal grid (cells, colors, attrs, hyperlinks) at interactive rates
- supports resize/DPR/zoom changes with stable geometry
- produces high-signal, machine-readable logs for correctness + perf gates
- fits the determinism + golden trace strategy for FrankenTerm (bd-lff4p)

Constraints:
- Safe Rust in-tree (`#![forbid(unsafe_code)]`).
- Deterministic behavior when requested (seed + explicit time source; trace replay).
- Correctness-first (no flicker, stable selection/hyperlinks).
- Performance: 120x40 steady-state at 60fps on modern hardware.

Related specs:
- Golden traces: `docs/spec/frankenterm-golden-trace-format.md` (bd-lff4p.5.1)
- North Star architecture: `docs/spec/frankenterm-architecture.md` (bd-lff4p.6)

## Decision

### D1) WebGPU-first renderer

Implement the renderer on WebGPU (via `wgpu`) with a single primary pipeline:
- per-cell instanced quads
- glyph alpha sampled from a cached atlas texture
- fg/bg/attrs applied in shader

We do NOT rely on DOM text rendering or canvas text for the final path.

### D2) Atlas-based glyph caching (monospace first)

Maintain an R8 (alpha) glyph atlas:
- glyph rasterization is done in WASM (pure Rust font rasterizer) to avoid browser-dependent glyph rendering.
- glyph cache is LRU-evicted under a fixed byte budget.

Monospace and a constrained shaping model are the initial target:
- terminal-class monospace font metrics
- grapheme clusters mapped to glyph runs via a stable, deterministic shaping path

### D3) Patch-driven updates (dirty spans)

The renderer consumes patches (dirty spans or diff runs) from the engine:
- only changed cells update instance buffer ranges (`queue.write_buffer` slices).
- unchanged cells do not trigger GPU work beyond the draw.

This aligns with the engine's determinism model: identical traces => identical patch stream.

Hard rule:
- `frankenterm-web` MUST NOT access the engine's full Grid directly; it only consumes the patch stream
  (plus explicit geometry/config inputs). This is required for trace replay (renderer can be driven from
  recorded patches) and prevents nondeterministic reads from leaking into the present path.

### D4) Correctness gates are patch/trace first; pixel gates are optional

Web pixel output can vary across GPU drivers. Therefore:
- primary correctness gates are **engine state** and **patch/frame hashes** from golden traces.
- pixel/framebuffer hashes are optional and must be bucketed by (DPR, size, renderer config) and treated as best-effort.

### D5) Present model

Use a standard swapchain surface:
- one draw call per frame (or a small constant number), plus optional overlay passes.
- avoid multi-pass compositing unless required (selection highlight, cursor, debug overlays can be done in-shader).

## Architecture

### Data flow

1. Engine produces a patch stream for the viewport:
   - dirty spans per row (preferred) or diff runs (compatible).
2. Renderer applies patch to CPU-side `Vec<CellInstance>` for the viewport.
3. Renderer uploads only modified instance slices to GPU.
4. Renderer issues instanced draw:
   - vertex shader: quad + position
   - fragment shader: sample atlas alpha + apply fg/bg/attrs

### Instance format (sketch)

Per cell:
- `x`, `y` (u16 or i32) in cell coords
- `glyph_id` (u32) into atlas metadata table
- `fg_rgba`, `bg_rgba` (packed u32)
- `attrs` (bitfield: bold/italic/underline/reverse/dim/...)
- `link_id` (optional u32) for hover/click mapping

### Geometry + DPR/zoom

The renderer owns a deterministic geometry model:
- inputs: container size, DPR, font metrics, user zoom
- outputs: (cols, rows), cell pixel size, origin offsets

Rules:
- cell size must be stable and reversible across resize storms
- mouse hit testing must use the same geometry mapping as rendering

## Alternatives Considered

### A1) Canvas2D / DOM text rendering

Pros:
- fast to prototype

Cons (reject):
- inconsistent glyph rasterization across browsers/OS
- harder to make deterministic gates
- performance cliffs under large scrollback + frequent updates

### A2) WebGL2

Pros:
- widely supported

Cons (reject):
- WebGPU is the strategic direction and provides better tooling and ergonomics

### A3) Precomposited textures per line / tile

Pros:
- can reduce per-cell instance work in some scenarios

Cons (defer):
- higher complexity; introduce only if profiling proves instancing is insufficient

## Consequences

### Positive
- Deterministic patch-level correctness gates even when pixel output varies.
- Scales with dirty spans: update cost proportional to changed area.
- GPU-friendly design (single pipeline, instancing, stable buffers).

### Negative / Costs
- Need a deterministic font rasterization approach in WASM.
- More up-front engineering than canvas-based approaches.
- Pixel-perfect goldens are not the primary gate (by design).

## Test Plan / Verification

Required (unit / wasm):
- atlas packer invariants (no overlap, bounded growth, LRU eviction correctness)
- geometry math tests (DPR/zoom/fit-to-container -> cols/rows mapping)
- patch application invariants (apply patch stream -> identical instance buffer)

Required (E2E / harness):
- web perf harness emitting JSON summary + JSONL detail (bd-lff4p.2.10)
- golden trace replay gate verifying patch/frame hashes (bd-lff4p.5.2)

Logging requirements:
- emit patch statistics per frame (dirty spans, bytes uploaded, draw calls)
- record DPR, font metrics, zoom, and renderer config in run header

## References

- bd-lff4p.2.1 (this ADR)
- bd-lff4p.2.4 (glyph rasterization + atlas cache)
- bd-lff4p.2.10 (web perf harness)
- bd-lff4p.5.1 (golden trace format)
- bd-lff4p.5.2 (trace replayer + checksum gates)
