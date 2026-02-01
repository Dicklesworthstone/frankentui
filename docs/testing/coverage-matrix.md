# Unit Test Coverage Matrix

This document encodes the project’s expectations for unit test coverage by crate and module.
It prevents “test later” drift, keeps kernel invariants continuously verified, and makes CI
decisions explicit.

See Bead: bd-3hy.

## How to Use
- When adding a new module, add it here.
- When adding a new public API, add explicit unit tests here.
- CI should enforce these thresholds (see bd-xn2).

## Coverage Targets (v1)
- ftui-render: ≥ 85% (kernel)
- ftui-core: ≥ 80% (terminal/session + input)
- ftui-style: ≥ 80%
- ftui-text: ≥ 80%
- ftui-layout: ≥ 75%
- ftui-runtime: ≥ 75%
- ftui-widgets: ≥ 70%
- ftui-extras: ≥ 60% (feature-gated)

Note: Integration-heavy PTY tests are enforced separately; do not “unit test” around reality.

## ftui-render (≥ 85%)
Kernel correctness lives here.

### Cell / CellContent / CellAttrs
- [ ] CellContent creation from char vs grapheme-id
- [ ] Width semantics (ASCII, wide, combining, emoji)
- [ ] Continuation-cell sentinel semantics for wide glyphs
- [ ] PackedRgba: construction + Porter-Duff alpha blending
- [ ] CellAttrs: bitflags operations + merge/override
- [ ] 16-byte Cell layout invariants (size/alignment) + bits_eq correctness

### Buffer
- [ ] Create/resize buffer with dimensions
- [ ] get/set bounds checking + deterministic defaults
- [ ] Clear semantics (full vs region)
- [ ] Scissor stack push/pop semantics (intersection monotonicity)
- [ ] Opacity stack push/pop semantics (product in [0,1])
- [ ] Wide glyph placement + continuation cells
- [ ] Iteration order and row-major storage assumptions

### Diff
- [ ] Empty diff (no changes)
- [ ] Single cell change
- [ ] Row changes
- [ ] Run grouping behavior
- [ ] Scratch buffer reuse (no unbounded allocations)

### Presenter
- [ ] Cursor tracking correctness
- [ ] Style tracking correctness
- [ ] Link tracking correctness (OSC 8 open/close)
- [ ] Single-write-per-frame behavior
- [ ] Synchronized output behavior where supported (fallback correctness)

## ftui-core (≥ 80%)

### Event types
- [ ] Canonical key/mouse/resize/paste/focus event types are stable

### InputParser
- [ ] Bounded CSI/OSC/DCS parsing (DoS limits)
- [ ] Bracketed paste decoding + max size
- [ ] Mouse SGR decoding
- [ ] Focus/resize event decoding

### TerminalCapabilities
- [ ] Env heuristic detection (TERM/COLORTERM)
- [ ] Mux flags (tmux/screen/zellij) correctness

### TerminalSession lifecycle
- [ ] RAII enter/exit discipline
- [ ] Panic cleanup paths are idempotent

## ftui-style (≥ 80%)
- [ ] Style defaults + builder ergonomics
- [ ] Deterministic style merge (explicit masks)
- [ ] Color downgrade (truecolor → 256 → 16 → mono)
- [ ] Theme presets + semantic slots
- [ ] StyleSheet registry + named style composition

## ftui-text (≥ 80%)
- [ ] Segment system correctness (Cow<str>)
- [ ] Width measurement correctness + LRU cache behavior
- [ ] Grapheme segmentation helpers for wrap/truncate correctness
- [ ] Wrap/truncate semantics for ZWJ/emoji/combining
- [ ] Markup parser correctness (feature-gated)

## ftui-layout (≥ 75%)
- [ ] Rect operations (intersection/contains)
- [ ] Flex constraint solving + gaps
- [ ] Grid placement + spanning + named areas
- [ ] Min/max sizing invariants

## ftui-runtime (≥ 75%)
- [ ] Deterministic scheduling (update/view loop)
- [ ] Cmd sequencing + cancellation
- [ ] Subscription polling correctness
- [ ] Simulator determinism (headless)

## ftui-widgets (≥ 70%)
- [ ] Harness-essential widgets have snapshot tests
- [ ] Widgets: key unit tests (render + layout invariants)

## ftui-extras (≥ 60%)
- [ ] Feature-gated modules include correctness tests
- [ ] Protocol detection tested (where applicable)
