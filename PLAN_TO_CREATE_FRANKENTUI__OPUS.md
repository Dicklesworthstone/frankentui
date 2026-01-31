# PLAN_TO_CREATE_FRANKENTUI__OPUS.md

## FrankenTUI (ftui): The Optimal Terminal UI Kernel

**Version 6.1 — Kernel-First, Math-Informed, Practice-Proven Architecture (Ultimate Hybrid Plan)**

> Design goal: **scrollback-native, zero-flicker, agent-ergonomic, and high-performance** Rust terminal apps
> (agent harnesses, REPLs, dashboards, pagers) built on a *tiny sacred kernel* plus feature-gated layers.

> **Document note (important):**
> Code blocks in this plan are **design sketches** for API shape, invariants, and responsibilities.
> They are intentionally "close to Rust," but not guaranteed to compile verbatim.
> The correctness contract lives in the invariants, tests, and ADRs—not in pseudo-code syntax.

> **Core promise:** ftui is designed so you can build a Claude Code / Codex-class agent harness UI
> without flicker, without cursor corruption, and without sacrificing native scrollback.

---

# PART 0: EXECUTIVE BLUEPRINT

## 0.1 Executive Summary

FrankenTUI ("ftui") is a deliberately minimal, high-performance terminal UI kernel that fuses the best
surviving abstractions from three Rust codebases (rich_rust, charmed_rust, opentui_rust) into one coherent system.

This is **not** a 1:1 port, and it is **not** "a widget zoo." ftui is:
- **Inline-first** by default (native scrollback preserved)
- **Flicker-free by construction** (diffed, buffered, state-tracked output; synchronized output when available)
- **Deterministic and testable** (simulator + snapshot tests + PTY correctness tests)
- **Fast under real loads** (cache-local buffers, pooled graphemes, minimal ANSI churn)
- **Layered** so the kernel stays small and stable while widgets/extras can evolve rapidly

### Primary "real world" targets
- **Agent harness UIs**: Claude Code / Codex-style interactive sessions with:
  - streaming output (log area) + stable UI chrome (status/tool/inputs)
  - native scrollback (inline mode) + optional alt-screen
  - mouse + links + selections
  - no flicker, no cursor corruption, reliable cleanup on crash
- **Traditional TUIs**: dashboards, monitors, pickers, forms
- **Export / replay** (optional): capture frames or segment streams for HTML/SVG/text export and tests

### 0.1.1 Non-Negotiables (Engineering Contract)

These are not aspirations; they are the **contract**. If any are violated, ftui is not ftui yet.

- **One writer owns the terminal**
  - All bytes that affect terminal state (cursor, modes, clears, SGR, OSC, etc.) are serialized through a single owner.
  - This is required for inline mode + streaming logs to be robust.

- **Diffed + buffered UI only**
  - No ad-hoc "println in the middle of render" patterns inside the UI path.
  - If the application wants logs, logs go through the ftui writer API (or a controlled capture/mux).

- **Inline-first is real**
  - Inline mode must preserve native scrollback and must not rely on alt-screen to look correct.
  - AltScreen remains opt-in and supported, including temporary modal use.

- **Safe by default, unsafe isolated**
  - `#![forbid(unsafe_code)]` in all core crates by default.
  - If SIMD/unsafe hot loops exist, they live in an explicitly separate feature-gated module or crate,
    with audit notes and benchmarks, and a scalar safe fallback always available.

- **Deterministic by design**
  - Rendering is pure given (model state + terminal size + theme).
  - Side effects happen only through a controlled command/event system or an explicit IO API.

- **Correctness is continuously verified**
  - PTY tests validate cleanup/cursor behavior on real terminals.
  - A terminal-model test harness validates presenter output.
  - Fuzzing keeps the input parser panic-free and bounded.

## 0.2 Mission and Non-Goals

### Mission
- Create the smallest possible **kernel** that makes terminal apps *pleasant and robust*:
  - input → canonical events → (optional runtime) → frame → diff → minimal ANSI output
- Provide an API surface that is **agent-ergonomic**:
  - easy to stream logs while showing structured UI
  - easy to build components without fighting the renderer
- Make correctness **provable by tests**:
  - cursor policy, inline mode invariants, width correctness, diff correctness

### Non-goals (explicit)
- Backwards compatibility with upstream APIs (we keep concepts, not types)
- A monolithic crate bundling every widget and integration
- "Prove global minimal ANSI output for all cases"
  - We instead target: *minimal enough* (grouped writes, style-run grouping, state tracking) and
    validate via measurable output-size baselines.
- Full terminal emulation (we target output correctness for supported sequences)

## 0.3 The Three-Ring Architecture (Kernel → Widgets → Extras)

ftui is organized into three rings:

1) **Kernel (sacred)**: stable, minimal, deterministic
   - Frame/Buffer/Cell primitives
   - Diff engine
   - ANSI presenter (state tracked)
   - Input parser + canonical Event
   - Capability detection
   - Screen modes (Inline vs AltScreen) policy

2) **Widgets (reusable)**: built strictly on kernel primitives
   - Panels, lists, tables, viewports, inputs, text areas
   - Hit testing for mouse interaction

3) **Extras (feature-gated)**:
   - Markdown, syntax highlighting, forms, SSH/PTY integration, export (HTML/SVG), images (optional)

This separation ensures the kernel stays "obviously correct" while UX layers can evolve without destabilizing it.

## 0.4 Workspace Layout (Crates)

Use a workspace (recommended) to enforce layering boundaries:

0) `ftui` (facade / public API)
   - The crate most apps depend on
   - Re-exports stable public types (`App`, `ScreenMode`, `Event`, `Frame`, `Style`, etc.)
   - Provides prelude, ergonomic builders, and sensible defaults
   - Keeps internal crate boundaries intact without leaking complexity to users

1) `ftui-core`
   - Raw mode + terminal lifecycle guards (RAII)
   - Capability detection (env heuristics + optional queries)
   - Input parsing into canonical `Event`
   - Screen mode policy helpers (Inline/AltScreen)

2) `ftui-render`
   - `Cell`, `Buffer`, `Frame` (Frame = buffer + optional hit grid + metadata)
   - `GraphemePool`, `LinkRegistry`, optional `HitGrid`
   - Diff engine (cell-level scan + run grouping)
   - `Presenter` (ANSI writer, state tracked, single write per frame)
   - Optional `simd` feature (unsafe isolated)

3) `ftui-style`
   - Visual styling model (`TextStyle`, `CellStyle`, `Theme`, semantic colors)
   - Color downgrade (TrueColor → 256 → 16 → ASCII)
   - Deterministic style merge semantics (mask/props)

4) `ftui-text`
   - `Text`, `Span`, `Segment` (+ markup parser as optional)
   - Width measurement, wrapping, truncation, alignment
   - Caches (LRU widths) and grapheme segmentation helpers

5) `ftui-layout`
   - `Rect`, constraints, row/col/grid layout, measurement protocol

6) `ftui-runtime` (optional ring-2 "standard runtime")
   - Bubbletea/Elm-like `Program`, `Model`, `Cmd`, scheduler/ticks
   - Deterministic simulator

7) `ftui-widgets` (feature-gated)
   - Component library on top of kernel + style/text/layout

8) `ftui-extras` (feature-gated)
   - Markdown, syntax highlighting, export, SSH, forms, etc.

9) `ftui-harness` (examples / demos)
   - Agent harness reference implementation

10) `ftui-simd` (optional, unsafe allowed, isolated)
   - Feature-gated implementations of hot loops (cell compare/clear/blend/width fast paths)
   - Strict rules:
     - depends on `ftui-render` (not the other way around)
     - must have scalar fallback
     - must have benches + targeted correctness tests
     - must document invariants and supported architectures

11) `ftui-pty` (test-only helper crate)
   - PTY spawning utilities and golden-output harness
   - Minimizes boilerplate and makes PTY tests consistent in CI

## 0.5 Kernel Invariants (Must Always Hold)

- **All UI rendering is diffed and buffered** (Frame/Buffer → diff → presenter). No ad-hoc writes.
- **Inline mode never clears the full screen** (preserve scrollback). Only clears a bounded UI region.
- **Cursor correctness**: after any present(), cursor position is restored per the active policy.
- **Style correctness**: presenter never leaves terminal in a "dangling" style/link state after exit.
- **Width correctness**: grapheme width accounting is correct for all drawn glyphs (including ZWJ sequences).
- **Input parsing is lossless** for supported sequences and robust against malformed input (limits enforced).
- **Cleanup is guaranteed**: raw mode, cursor visibility, bracketed paste, mouse modes, alt screen are restored
  even on panic (panic hook + RAII guards).
- **Single-writer rule**: ftui owns and serializes terminal output for correctness in inline mode.
  - If external code writes directly to stdout/stderr concurrently, inline-mode behavior is undefined unless
    the app routes that output through ftui's mux/capture APIs.
  - This is a deliberate constraint to keep guarantees meaningful.
- **Unsafe isolation**: core crates are safe-by-default (`forbid(unsafe_code)`); any unsafe lives in `ftui-simd`
  or similarly isolated module with explicit tests + benchmarks.

## 0.6 Design Decisions to Lock Early (Decision Records / ADRs)

1) **Frame is the canonical render target**
   - Segment/text pipelines are intermediates; everything converges to Frame for diffing.

2) **Inline-first**
   - AltScreen is explicit, opt-in, and never required to use the library.

3) **Presenter emits grouped runs**
   - Changes are emitted in row-major runs (not per-cell cursor move) to reduce ANSI bytes and reduce flicker.

4) **Unsafe containment policy**
   - Default build uses fully safe scalar paths.
   - `simd` / "hot loops" can use unsafe but MUST be:
     - isolated in one module/crate
     - feature gated
     - tested + benchmarked
     - audited (document invariants; forbid "creative" unsafe)

5) **Style is split by responsibility**
   - `CellStyle` is tiny and renderer-facing (packed fg/bg/attrs/link-id).
   - Higher-level style (theme/semantic colors/layout) resolves into `CellStyle` before drawing.

6) **Public API shape**
   - Provide a cohesive `ftui::App` / `ftui::Program` entrypoint with a stable ergonomic surface.
   - Core crates remain composable for advanced usage, but the common path is obvious and pleasant.

7) **Terminal backend strategy**
   - Choose a default backend (cross-platform) for raw mode + input + resize.
   - Allow swapping backends via feature flags if needed (but do not over-abstract prematurely).
   - The plan must explicitly decide: "what do we ship first" vs "what stays optional."

8) **Inline-mode implementation strategy**
   - Decide whether the primary mechanism is:
     - scroll-region margins (DECSTBM) to keep UI pinned while logs scroll, or
     - save/restore overlay redraw, or
     - hybrid with capability-based selection.
   - This decision gets an ADR because it drives nearly every "agent harness" behavior.

9) **Concurrency and IO model**
   - Decide whether ftui supports:
     - single-threaded ownership only, or
     - optional render thread / output thread.
   - Either way, the **one-writer rule** remains mandatory.

10) **Span/style precedence**
   - Explicitly define overlap semantics for text spans and markup.
   - Determinism here prevents "heisenbugs" in rendering and snapshots.

## 0.7 Quality Gates (Stop-Ship if Failing)

- **Gate 1: Inline mode stability**
  - Re-rendering UI region while streaming logs cannot corrupt scrollback or cursor placement.

- **Gate 2: Diff/presenter correctness**
  - Property tests: applying presenter output to a terminal-model yields the expected grid for supported ops.

- **Gate 3: Unicode width correctness**
  - Test suite includes emoji/ZWJ/combining marks; no off-by-one wrapping errors allowed.

- **Gate 4: Terminal cleanup**
  - PTY tests verify raw mode + cursor visibility + alt screen restoration after normal exit and panic.

## 0.8 Public API Surface (Target)

ftui succeeds or fails based on how it feels to build real apps—especially **agent harness UIs**.
The kernel can remain minimal while the facade provides a coherent "pit of success."

### 0.8.1 Canonical entrypoint: `ftui::App`

Design goals:
- An obvious default path that "just works" for inline + scrollback + stable UI region.
- Advanced control remains available via `ftui-core` / `ftui-render` without fighting the facade.

Sketch:
```rust
use ftui::{App, Cmd, Event, Frame, Model, ScreenMode, Result};

struct State { count: u64 }

impl Model for State {
    type Message = Event;

    fn update(&mut self, msg: Event) -> Cmd<Event> {
        match msg {
            Event::Key(k) if k.is_char('q') => Cmd::quit(),
            Event::Key(k) if k.is_char('+') => { self.count += 1; Cmd::none() }
            _ => Cmd::none(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        // draw into frame.buffer using text/style/layout helpers
    }
}

fn main() -> Result<()> {
    App::new(State { count: 0 })
        .screen_mode(ScreenMode::Inline { ui_height: 6 })
        .run()
}
```

### 0.8.2 "Easy mode" adapter: `view_string()`

Support trivial apps and prototyping without violating kernel discipline:
- Accept a `String` (or `Text`) and internally route it through:
  `String -> Text/Segments -> Frame -> Diff -> Presenter`.

### 0.8.3 Agent harness oriented API

The reference harness must not be "just an example"; it is the design forcing function.

Minimum harness primitives:
- `write_log(text)` that is scrollback-native (inline mode)
- `present_ui(frame)` that is atomic and flicker-resistant
- optional "modal alt-screen" for complex UI interactions
- child-process capture via PTY (feature gated) to guarantee the one-writer rule

## 0.9 Output + Concurrency Architecture (Critical for Inline Mode)

Inline mode plus streaming output is the hardest real-world requirement. Most flicker/corruption bugs
come from broken assumptions about terminal ownership and write interleaving.

ftui solves this by making the IO topology explicit.

### 0.9.1 The One-Writer Rule

- Exactly one component is allowed to emit bytes that mutate terminal state.
- That component is either:
  - a `TerminalWriter` in the same thread, or
  - a dedicated render/output thread that owns the `Presenter`.

Everything else communicates via messages (events/commands) or a controlled logging API.

### 0.9.2 Two supported operating modes

#### Mode A: Single-threaded (simplest, default)
- The runtime loop owns the `Presenter` and writes to terminal synchronously.
- `write_log()` and `present_ui()` are serialized by the runtime.
- Best for smaller apps and simplest mental model.

#### Mode B: Dedicated render/output thread (optional, feature gated)
- A render thread owns:
  - `Presenter`
  - `TerminalSession` output handle
  - `prev_buffer` (front buffer)
- The runtime sends messages:
  - `Render(frame_buffer)`
  - `Log(bytes or lines)`
  - `SetMode(...)`, `Shutdown`, etc.
- Guarantees:
  - no interleaving of writes
  - optional backpressure (drop frames, coalesce logs)
  - the runtime stays responsive under heavy IO

### 0.9.3 Atomic present contract

Goal: minimize flicker and tearing by ensuring "present" is emitted as a single buffered operation.

Practical contract:
- `Presenter::present()` performs:
  - optional synchronized output begin/end
  - run-grouped emission
  - flush
- Buffered writer is sized for worst-case frame bytes (practically 64KB+; tuneable).

### 0.9.4 Output mux / capture (agent harness realism)

To build reliable harnesses, ftui should support routing external output through the one-writer.

Feature-gated options:
- PTY capture for child processes:
  - spawn tool subprocesses under PTY
  - read their stdout/stderr streams
  - write as log stream through ftui (preserving scrollback)
- In-process log routing:
  - provide a `LogSink` or `Writer` that application code uses
  - discourage raw `println!` during inline mode

## 0.10 Open Questions (Resolve Early; Track as ADRs)

The plan is intentionally ambitious; these are the few decisions that most often cause multi-week churn
if not resolved early.

1) **Inline mode anchoring and scroll behavior**
   - Bottom-anchored UI region is preferred for harness UIs, but the exact scroll strategy matters.
   - Decide primary strategy: scroll-region vs overlay redraw vs hybrid.

2) **Terminal backend choice**
   - Which library is responsible for raw mode, resize detection, and reading input on:
     - Unix terminals
     - Windows terminal
   - Prefer a backend that is stable and widely used.

3) **Async commands model**
   - Feature-gated:
     - thread pool (std-only), or
     - tokio integration
   - Ensure deterministic simulator remains possible regardless.

4) **Text span precedence**
   - When spans overlap:
     - "later wins" is deterministic but must be documented
     - define how masks/explicit properties merge

5) **Presenter style emission strategy**
   - Full reset (`SGR 0`) then apply is simple and correct but may bloat output.
   - Incremental SGR emission reduces bytes but increases complexity.
   - Decide and benchmark with real workloads (harness logs + UI chrome).

6) **Windows support scope (v1)**
   - Decide what is supported in v1 and what is "best effort."
   - Document feature gaps (OSC 8, sync output, mouse reporting differences).

## 0.11 Definition of Done (v1)

ftui "v1" is done when these are true:

- Inline mode default is stable:
  - streaming logs + UI chrome does not corrupt scrollback or cursor
  - PTY tests confirm behavior on normal exit and panic
- Diff/presenter correctness validated by:
  - terminal-model tests for supported sequences
  - output size baselines for representative workloads
- Unicode width correctness proven by corpus tests (ZWJ, emoji, combining marks)
- Style system is deterministic and documented (explicit masks + inheritance)
- Runtime supports:
  - update/view loop
  - ticks
  - batch/sequence commands
  - deterministic simulator + snapshot tests
- Core widgets needed for harness UIs exist and are tested:
  - viewport/log viewer
  - status line / panel
  - text input / prompt line
  - progress/spinner
- Documentation includes:
  - "build an agent harness UI" tutorial
  - inline vs alt-screen explanation
  - IO ownership and one-writer rule guidance

---

# PART I: FOUNDATIONS

## Chapter 1: First-Principles Derivation

### 1.1 What IS a Terminal?

A terminal is a **state machine** that transforms a stream of bytes into a visual grid of styled characters:

```
Terminal: (State, Byte*) → (State', Grid)
where:
  State = { cursor: (x,y), style: Style, mode: Mode, ... }
  Grid = Cell[width × height]
  Cell = { char: Grapheme, fg: Color, bg: Color, attrs: Attributes }
```

**Fundamental theorem**: Any terminal UI library is an **inverse function** — it must produce the minimal byte stream that transforms the current grid into the desired grid:

```
Render: (Grid_current, Grid_desired) → Byte*
such that Terminal(State, Render(G_c, G_d)) = (_, G_d)
```

This immediately implies (as a guiding principle):
1. **Diff-based rendering is optimal** — producing bytes for unchanged cells is wasteful
2. **State tracking is essential** — we must know the terminal's current state to emit minimal sequences
3. **Cell equality must be fast** — we compare O(w×h) cells per frame

⚠️ Practical note: "globally minimal" ANSI output is not always attainable without solving
hard sequencing problems (cursor moves, clears, style resets, terminal quirks).
ftui's objective is:
- **correct output**
- **bounded flicker**
- **near-minimal bytes** via:
  - row-major run grouping
  - style-run coalescing
  - state-tracked SGR and cursor position
  - heuristics (full redraw vs diff)

### 1.2 The Fundamental Operations

From first principles, a terminal UI library needs exactly these operations:

| Operation | Mathematical Model | Complexity Target |
|-----------|-------------------|-------------------|
| **Cell comparison** | `Cell × Cell → Bool` | O(1), < 1ns |
| **Cell mutation** | `Grid × (x,y) × Cell → Grid` | O(1), < 5ns |
| **Diff computation** | `Grid × Grid → [(x,y,Cell)]` | O(w×h), < 500µs for 80×24 |
| **ANSI emission** | `[(x,y,Cell)] → Byte*` | O(changes), minimal bytes |
| **Style composition** | `Style × Style → Style` | O(1), < 50ns |
| **Text measurement** | `String → usize` | O(graphemes), cached |
| **Grapheme interning** | `String → GraphemeId` | O(1) amortized |
| **Alpha blending** | `Rgba × Rgba → Rgba` | O(1), < 10ns |

### 1.3 The Optimality Conditions

In practice, terminals are messy and "global optimality" for ANSI emission is both hard to define and
rarely worth the engineering complexity. ftui instead targets **practical optimality**:

1. **Correct output under real terminals** (primary)
2. **Flicker resistance** via buffered atomic present and (when available) synchronized output
3. **Near-minimal bytes** where it matters (run grouping, state tracking, sensible heuristics)
4. **Low and predictable latency** (bounded CPU + bounded output)
5. **Cache-efficient data layout** (so we can hit high FPS without drama)

### Practical corollary: required architecture choices

The architecture must:
- Use cell-level diffing (not region-level)
- Track terminal state to avoid redundant SGR codes
- Use fast cell equality (compiler-vectorized scalar comparisons are fine; SIMD is optional)
- Cache computed values (widths, style resolution)
- Pool complex content (graphemes, links)

**Additional real-world constraints (often ignored by "optimal" proofs):**
- Terminals differ in support and correctness; we must feature-detect and provide fallbacks.
- "No flicker" depends on:
  - synchronized output support, and/or
  - a strict output policy (single buffered write per frame) and avoiding full-screen clears in inline mode.
- Robustness depends on cleanup discipline and crash recovery.
- Correctness depends on IO ownership; without the one-writer rule, guarantees become meaningless.

### 1.4 The Three-Library Synthesis

FrankenTUI extracts the optimal kernel from three Rust TUI libraries:

| Source | Contribution | Why Optimal |
|--------|--------------|-------------|
| **opentui_rust** | Cell grid, diff algorithm, alpha blending, scissor/opacity stacks, grapheme pool | Cache-optimal 16-byte cells, bitwise comparison, Porter-Duff compositing |
| **rich_rust** | Segment abstraction, markup parser, measurement protocol, Renderable trait, Live display | Cow<str> for zero-copy, event-driven span rendering, LRU width cache |
| **charmed_rust** | LipGloss styling, CSS-like properties, theme system, Elm architecture | Bitflags property tracking, shorthand tuple conversion, adaptive colors |

### 1.5 What Survives vs What Gets Dropped (Explicit)

This plan keeps only what strengthens the kernel:

**From opentui_rust (survives)**
- Cache-local buffers, pooled graphemes, scissor/opacity, alpha blending (opt-in)
- State-tracked ANSI presenter
- Input parsing patterns and safety limits
- Hit testing grid concept (mouse affordances)

**From rich_rust (survives)**
- Segment/text modeling and measurement protocol (min/max)
- Markup parsing (optional layer, not kernel)
- Export-friendly intermediate representations (Segments as optional output format)

**From charmed_rust (survives)**
- Bubbletea/Elm runtime model as an *optional* standard runtime crate
- Lipgloss-like ergonomics (builders), BUT with responsibility-separated structs
- Semantic theme slots and style inheritance semantics (explicit masks/props)

**Dropped / de-scoped from kernel**
- Widget zoo, markdown, syntax highlighting, forms: moved to feature-gated layers
- "View returns String" as the only API: supported via adapter, not as the core abstraction

---

## Chapter 2: Formal Specifications

### 2.1 Terminal State Machine (TLA+-style)

```tla
---------------------------- MODULE Terminal ----------------------------
VARIABLES cursor, style, grid, mode

TypeInvariant ==
    /\ cursor \in (0..width-1) × (0..height-1)
    /\ style \in Style
    /\ grid \in [0..width-1 × 0..height-1 -> Cell]
    /\ mode \in {Normal, Raw, AltScreen}

Init ==
    /\ cursor = (0, 0)
    /\ style = DefaultStyle
    /\ grid = [p \in 0..width-1 × 0..height-1 |-> EmptyCell]
    /\ mode = Normal

ProcessByte(b) ==
    CASE IsControlChar(b) -> HandleControl(b)
      [] IsEscapeStart(b) -> ParseEscape
      [] IsPrintable(b)   ->
           /\ grid' = [grid EXCEPT ![cursor] = Cell(b, style)]
           /\ cursor' = AdvanceCursor(cursor)

SafetyInvariant ==
    \A p \in Domain(grid): grid[p] \in Cell

LivenessProperty ==
    <>[](\A desired: Eventually(grid = desired))
=========================================================================
```

### 2.2 Rendering Pipeline State Machine

```
States: {Idle, Measuring, Rendering, Diffing, Presenting, Error}

Transitions:
  Idle --[render_request]--> Measuring
  Measuring --[layout_complete]--> Rendering
  Rendering --[draw_complete]--> Diffing
  Diffing --[diff_complete]--> Presenting
  Presenting --[present_complete]--> Idle
  * --[error]--> Error
  Error --[recover]--> Idle

Invariants:
  I1: In Rendering state, only back buffer is modified
  I2: In Presenting state, only ANSI output is produced
  I3: After Presenting, front buffer = desired grid
  I4: Error state restores terminal to safe state
  I5: Scissor stack intersection monotonically decreases on push
  I6: Opacity stack product stays in [0, 1]
```

### 2.3 Buffer Invariants (Formal)

```rust
/// INVARIANT: Buffer dimensions never change after creation
/// PROOF: width/height are immutable; cells vec size = width * height

/// INVARIANT: Cell access is always in bounds
/// PROOF: get/set check x < width && y < height; unchecked requires unsafe

/// INVARIANT: Scissor stack intersection is monotonically decreasing
/// PROOF: push() computes intersection; pop() restores previous (saved on stack)

/// INVARIANT: Opacity stack product is in [0, 1]
/// PROOF: values clamped on push; product = Π(stack) where each factor ∈ [0,1]

/// INVARIANT: GraphemePool slot.refcount > 0 implies slot is valid
/// PROOF: intern() sets refcount = 1; incref() adds 1; decref() only frees at 0

/// INVARIANT: Cell.content.Grapheme(id) implies id is valid in associated pool
/// PROOF: GraphemeId only created via pool.intern(); invalidation tracked
```

---

## Chapter 3: Cache Hierarchy Analysis

### 3.1 Memory Layout for Cache Efficiency

Modern CPUs have:
- **L1 cache**: 32-64 KB, ~4 cycles latency
- **L2 cache**: 256 KB - 1 MB, ~12 cycles
- **L3 cache**: 8-32 MB, ~40 cycles
- **Cache line**: 64 bytes

**Design implications**:

```
Cell = 16 bytes
  → 4 cells per cache line
  → 80×24 grid = 1920 cells = 30,720 bytes (fits in L1!)
  → 200×60 grid = 12000 cells = 192 KB (fits in L2)

Optimal access pattern:
  - Row-major iteration (cells[y * width + x])
  - Sequential scan for diff (no random access)
  - Prefetch next row while processing current
  - Avoid pointer chasing (use indices into arrays)
```

### 3.2 Cell Layout: The 16-Byte Sweet Spot

```rust
// OPTIMAL: 16 bytes, exactly 4 per 64-byte cache line
// Memory layout:
// ┌─────────────────┬─────────────────┬─────────────────┬─────────────────┐
// │  content (4B)   │    fg (4B)      │    bg (4B)      │   attrs (4B)    │
// └─────────────────┴─────────────────┴─────────────────┴─────────────────┘
// Offset:  0              4                 8                 12

#[repr(C, align(16))]
pub struct Cell {
    content: CellContent,      // 4 bytes: char or GraphemeId
    fg: PackedRgba,            // 4 bytes: RGBA as u32
    bg: PackedRgba,            // 4 bytes: RGBA as u32
    attrs: CellAttrs,          // 4 bytes: flags (8 bits) + link_id (24 bits)
}

// WHY NOT 24 or 32 bytes?
// - 24 bytes: 2.67 cells/line, wastes 16 bytes per line
// - 32 bytes: 2 cells/line, doubles memory bandwidth
// - 16 bytes: Perfect fit, single SIMD comparison possible
```

### 3.3 Comparison: What We Avoid

```rust
// ❌ SUBOPTIMAL: 48+ bytes, heap allocations
pub struct CellBad {
    content: String,   // 24 bytes (ptr + len + cap) + HEAP
    fg: Rgba,          // 16 bytes (4 × f32)
    bg: Rgba,          // 16 bytes
    attrs: u64,        // 8 bytes
}
// Problems:
// - 1.3 cells per cache line
// - Heap allocation for every cell
// - Pointer chasing for content comparison

// ✅ OPTIMAL: 16 bytes, no heap for single chars
pub struct Cell {
    content: CellContent,  // Enum: Empty | Char(char) | Grapheme(GraphemeId)
    fg: PackedRgba,        // Packed u32
    bg: PackedRgba,        // Packed u32
    attrs: CellAttrs,      // Bitfield u32
}
// Benefits:
// - 4 cells per cache line
// - No heap for 99% of cells (ASCII/BMP chars)
// - Single 128-bit comparison
```

---

## Chapter 4: SIMD Opportunities

### 4.1 Where SIMD Helps

| Operation | SIMD Potential | Expected Speedup |
|-----------|---------------|------------------|
| Cell comparison | 4 cells at once (64 bytes = 4×16) | 4× |
| Buffer clear | 256-bit writes | 8× |
| Color blending | 4 RGBA channels at once | 4× |
| ASCII width | 32 chars at once with AVX2 | 32× |
| Row copy | Entire rows with AVX-512 | 4× |

### 4.2 Cell Comparison with SIMD

```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Compare 4 cells at once (64 bytes) using AVX2
/// Returns bitmask where bit i is set if cells[i] differs
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn compare_cells_avx2(old: &[Cell; 4], new: &[Cell; 4]) -> u8 {
    // Load 64 bytes (4 cells) from each buffer
    let old_lo = _mm256_loadu_si256(old.as_ptr() as *const __m256i);
    let old_hi = _mm256_loadu_si256((old.as_ptr() as *const __m256i).add(1));
    let new_lo = _mm256_loadu_si256(new.as_ptr() as *const __m256i);
    let new_hi = _mm256_loadu_si256((new.as_ptr() as *const __m256i).add(1));

    // Compare 32 bytes at a time
    let cmp_lo = _mm256_cmpeq_epi64(old_lo, new_lo);
    let cmp_hi = _mm256_cmpeq_epi64(old_hi, new_hi);

    // Extract mask: 1 means equal
    let mask_lo = _mm256_movemask_epi8(cmp_lo) as u32;
    let mask_hi = _mm256_movemask_epi8(cmp_hi) as u32;

    // Convert to per-cell mask (4 bits for 4 cells)
    // Each cell is 16 bytes = 2 qwords, so check pairs
    let cell_mask =
        (((mask_lo & 0xFFFF) == 0xFFFF) as u8) |        // Cell 0
        ((((mask_lo >> 16) & 0xFFFF) == 0xFFFF) as u8) << 1 |  // Cell 1
        (((mask_hi & 0xFFFF) == 0xFFFF) as u8) << 2 |   // Cell 2
        ((((mask_hi >> 16) & 0xFFFF) == 0xFFFF) as u8) << 3;   // Cell 3

    // Invert: 1 means DIFFERENT
    !cell_mask & 0x0F
}

/// Scalar fallback for non-AVX2 systems (ALWAYS AVAILABLE)
#[inline]
fn compare_cells_scalar(old: &Cell, new: &Cell) -> bool {
    // Safe version: compare as 4 u32 values
    // Compiler can vectorize this
    old.content != new.content ||
    old.fg != new.fg ||
    old.bg != new.bg ||
    old.attrs != new.attrs
}
```

**NOTE ON UNSAFE POLICY:**
The plan originally used `unsafe` transmute/read for bitwise comparisons.
In ftui v5, we require:
- a safe default implementation (compiler can vectorize 4×u32 compares)
- optional `simd` feature uses isolated unsafe for AVX/SSE paths

This preserves Opus-level performance without making the whole codebase "unsafe by default".

### 4.3 ASCII Width with SIMD

```rust
/// Check if string is pure ASCII and return width in one pass
/// For ASCII, width = byte length (huge optimization)
#[cfg(all(target_arch = "x86_64", feature = "simd"))]
#[target_feature(enable = "avx2")]
unsafe fn ascii_width_simd(s: &[u8]) -> Option<usize> {
    let len = s.len();
    let mut i = 0;

    // Process 32 bytes at a time
    let high_bit_mask = _mm256_set1_epi8(0x80u8 as i8);

    while i + 32 <= len {
        let chunk = _mm256_loadu_si256(s[i..].as_ptr() as *const __m256i);
        let high_bits = _mm256_and_si256(chunk, high_bit_mask);
        let any_high = _mm256_testz_si256(high_bits, high_bits);
        if any_high == 0 {
            return None;  // Non-ASCII byte found
        }
        i += 32;
    }

    // Handle remainder with scalar
    for &b in &s[i..] {
        if b & 0x80 != 0 { return None; }
    }

    Some(len)  // ASCII width = byte length
}

/// Safe scalar fallback (always available)
fn ascii_width_scalar(s: &[u8]) -> Option<usize> {
    if s.iter().all(|&b| b & 0x80 == 0) {
        Some(s.len())
    } else {
        None
    }
}
```

---

## Chapter 5: Core Type Implementations

### 5.1 CellContent: Grapheme Encoding

```rust
/// Cell content: char (BMP) or GraphemeId (complex sequences)
/// Layout: [31: type bit][30-0: data]
/// - If bit 31 == 0: char (Unicode scalar value up to U+7FFFFFFF)
/// - If bit 31 == 1: GraphemeId (pool slot in bits 0-23, width in bits 24-30)
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct CellContent(u32);

impl CellContent {
    pub const EMPTY: Self = Self(0);

    /// Create from single char (fast path for ASCII/BMP)
    #[inline]
    pub fn from_char(c: char) -> Self {
        Self(c as u32)
    }

    /// Create from GraphemeId (for complex sequences like emoji)
    #[inline]
    pub fn from_grapheme(id: GraphemeId) -> Self {
        Self(0x8000_0000 | id.0)
    }

    /// Is this a complex grapheme (not single char)?
    #[inline]
    pub fn is_grapheme(&self) -> bool {
        self.0 & 0x8000_0000 != 0
    }

    /// Display width (1 for most chars, 2 for wide, encoded for graphemes)
    #[inline]
    pub fn width(&self) -> usize {
        if self.is_grapheme() {
            ((self.0 >> 24) & 0x7F) as usize
        } else if self.0 == 0 {
            0
        } else {
            unicode_width::UnicodeWidthChar::width(char::from_u32(self.0).unwrap_or(' '))
                .unwrap_or(1)
        }
    }
}

/// Grapheme ID: reference to interned string in GraphemePool
/// Layout: [30-24: width (7 bits)][23-0: pool slot (24 bits)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct GraphemeId(u32);

impl GraphemeId {
    #[inline]
    pub fn new(slot: u32, width: u8) -> Self {
        debug_assert!(slot < 0x00FF_FFFF, "slot overflow");
        debug_assert!(width < 128, "width overflow");
        Self((slot & 0x00FF_FFFF) | ((width as u32) << 24))
    }

    #[inline]
    pub fn slot(&self) -> usize {
        (self.0 & 0x00FF_FFFF) as usize
    }

    #[inline]
    pub fn width(&self) -> usize {
        ((self.0 >> 24) & 0x7F) as usize
    }
}
```

### 5.2 PackedRgba: Color with Alpha

```rust
/// RGBA color packed into 4 bytes
/// Layout: [R:8][G:8][B:8][A:8] (native endian)
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PackedRgba(pub u32);

impl PackedRgba {
    pub const TRANSPARENT: Self = Self(0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 255)
    }

    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32))
    }

    #[inline]
    pub fn r(&self) -> u8 { (self.0 >> 24) as u8 }
    #[inline]
    pub fn g(&self) -> u8 { (self.0 >> 16) as u8 }
    #[inline]
    pub fn b(&self) -> u8 { (self.0 >> 8) as u8 }
    #[inline]
    pub fn a(&self) -> u8 { self.0 as u8 }

    /// Porter-Duff "over" compositing: src over dst
    /// Result = src + dst × (1 - src.alpha)
    #[inline]
    pub fn over(self, dst: Self) -> Self {
        let src_a = self.a() as u32;
        if src_a == 255 { return self; }
        if src_a == 0 { return dst; }

        let inv_a = 255 - src_a;
        let r = ((self.r() as u32 * 255) + (dst.r() as u32 * inv_a)) / 255;
        let g = ((self.g() as u32 * 255) + (dst.g() as u32 * inv_a)) / 255;
        let b = ((self.b() as u32 * 255) + (dst.b() as u32 * inv_a)) / 255;
        let a = src_a + ((dst.a() as u32 * inv_a) / 255);

        Self::rgba(r as u8, g as u8, b as u8, a as u8)
    }

    /// Apply uniform opacity [0.0, 1.0]
    #[inline]
    pub fn with_opacity(self, opacity: f32) -> Self {
        let a = ((self.a() as f32) * opacity.clamp(0.0, 1.0)) as u8;
        Self((self.0 & 0xFFFF_FF00) | (a as u32))
    }
}
```

### 5.3 CellAttrs: Packed Attributes

```rust
/// Cell attributes packed into 4 bytes
/// Layout: [31-24: flags][23-0: link_id]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct CellAttrs(u32);

bitflags::bitflags! {
    /// Text style flags (8 bits)
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct StyleFlags: u8 {
        const BOLD          = 0b0000_0001;
        const DIM           = 0b0000_0010;
        const ITALIC        = 0b0000_0100;
        const UNDERLINE     = 0b0000_1000;
        const BLINK         = 0b0001_0000;
        const REVERSE       = 0b0010_0000;
        const STRIKETHROUGH = 0b0100_0000;
        const HIDDEN        = 0b1000_0000;
    }
}

impl CellAttrs {
    pub const NONE: Self = Self(0);

    #[inline]
    pub fn new(flags: StyleFlags, link_id: u32) -> Self {
        debug_assert!(link_id < 0x00FF_FFFF, "link_id overflow");
        Self(((flags.bits() as u32) << 24) | (link_id & 0x00FF_FFFF))
    }

    #[inline]
    pub fn flags(&self) -> StyleFlags {
        StyleFlags::from_bits_truncate((self.0 >> 24) as u8)
    }

    #[inline]
    pub fn link_id(&self) -> u32 {
        self.0 & 0x00FF_FFFF
    }

    #[inline]
    pub fn with_flags(self, flags: StyleFlags) -> Self {
        Self((self.0 & 0x00FF_FFFF) | ((flags.bits() as u32) << 24))
    }

    #[inline]
    pub fn with_link(self, link_id: u32) -> Self {
        Self((self.0 & 0xFF00_0000) | (link_id & 0x00FF_FFFF))
    }
}
```

### 5.4 Complete Cell Implementation

```rust
/// The atomic unit of terminal display: 16 bytes
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Cell {
    pub content: CellContent,
    pub fg: PackedRgba,
    pub bg: PackedRgba,
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            content: CellContent::EMPTY,
            fg: PackedRgba::WHITE,
            bg: PackedRgba::TRANSPARENT,
            attrs: CellAttrs::NONE,
        }
    }
}

impl Cell {
    /// Continuation cell for wide characters (placeholder)
    pub const CONTINUATION: Self = Self {
        content: CellContent(0xFFFF_FFFF),
        fg: PackedRgba::TRANSPARENT,
        bg: PackedRgba::TRANSPARENT,
        attrs: CellAttrs::NONE,
    };

    #[inline]
    pub fn is_continuation(&self) -> bool {
        self.content.0 == 0xFFFF_FFFF
    }

    /// Bitwise equality (fast path)
    #[inline]
    pub fn bits_eq(&self, other: &Self) -> bool {
        self.content == other.content &&
        self.fg == other.fg &&
        self.bg == other.bg &&
        self.attrs == other.attrs
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.content.width()
    }
}

impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        self.bits_eq(other)
    }
}

impl Eq for Cell {}
```

---

## Chapter 6: Buffer and Frame Implementation

### 6.1 Buffer: The Grid

```rust
/// Double-buffered grid of cells
pub struct Buffer {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
    scissor_stack: Vec<Rect>,
    opacity_stack: Vec<f32>,
}

impl Buffer {
    pub fn new(width: u16, height: u16) -> Self {
        let size = (width as usize) * (height as usize);
        Self {
            width,
            height,
            cells: vec![Cell::default(); size],
            scissor_stack: vec![Rect::new(0, 0, width, height)],
            opacity_stack: vec![1.0],
        }
    }

    #[inline]
    pub fn width(&self) -> u16 { self.width }

    #[inline]
    pub fn height(&self) -> u16 { self.height }

    /// Get cell at (x, y), returns None if out of bounds
    #[inline]
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        if x < self.width && y < self.height {
            Some(&self.cells[(y as usize) * (self.width as usize) + (x as usize)])
        } else {
            None
        }
    }

    /// Get cell at (x, y) without bounds check
    /// # Safety: x < width && y < height
    #[inline]
    pub fn get_unchecked(&self, x: u16, y: u16) -> &Cell {
        &self.cells[(y as usize) * (self.width as usize) + (x as usize)]
    }

    /// Set cell at (x, y), respecting scissor and opacity
    pub fn set(&mut self, x: u16, y: u16, mut cell: Cell) {
        if !self.in_scissor(x, y) { return; }

        // Apply opacity stack
        let opacity = self.current_opacity();
        if opacity < 1.0 {
            cell.fg = cell.fg.with_opacity(opacity);
            cell.bg = cell.bg.with_opacity(opacity);
        }

        // Apply alpha compositing if bg has transparency
        if cell.bg.a() < 255 {
            if let Some(existing) = self.get(x, y) {
                cell.bg = cell.bg.over(existing.bg);
            }
        }

        let idx = (y as usize) * (self.width as usize) + (x as usize);
        self.cells[idx] = cell;

        // Handle wide characters
        if cell.width() == 2 && x + 1 < self.width {
            self.cells[idx + 1] = Cell::CONTINUATION;
        }
    }

    /// Push scissor rect (intersection with current)
    pub fn push_scissor(&mut self, rect: Rect) {
        let current = self.current_scissor();
        let intersected = current.intersection(&rect);
        self.scissor_stack.push(intersected);
    }

    /// Pop scissor rect
    pub fn pop_scissor(&mut self) {
        if self.scissor_stack.len() > 1 {
            self.scissor_stack.pop();
        }
    }

    /// Push opacity (multiplicative with current)
    pub fn push_opacity(&mut self, opacity: f32) {
        let current = self.current_opacity();
        self.opacity_stack.push(current * opacity.clamp(0.0, 1.0));
    }

    /// Pop opacity
    pub fn pop_opacity(&mut self) {
        if self.opacity_stack.len() > 1 {
            self.opacity_stack.pop();
        }
    }

    #[inline]
    fn current_scissor(&self) -> Rect {
        *self.scissor_stack.last().unwrap()
    }

    #[inline]
    fn current_opacity(&self) -> f32 {
        *self.opacity_stack.last().unwrap()
    }

    #[inline]
    fn in_scissor(&self, x: u16, y: u16) -> bool {
        self.current_scissor().contains(x, y)
    }

    /// Clear buffer to default cells
    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
    }

    /// Raw slice access for diffing
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }
}

/// Frame = Buffer + metadata for a render pass
pub struct Frame {
    pub buffer: Buffer,
    pub hit_grid: Option<HitGrid>,
    pub cursor_position: Option<(u16, u16)>,
    pub cursor_visible: bool,
}
```

### 6.2 Diff Engine

```rust
/// Diff between two buffers: list of changed positions
pub struct BufferDiff {
    changes: Vec<(u16, u16)>,
}

impl BufferDiff {
    /// Compute diff between old and new buffers
    pub fn compute(old: &Buffer, new: &Buffer) -> Self {
        debug_assert_eq!(old.width(), new.width());
        debug_assert_eq!(old.height(), new.height());

        let mut changes = Vec::new();
        let width = old.width();
        let height = old.height();

        // Row-major scan for cache efficiency
        for y in 0..height {
            for x in 0..width {
                let old_cell = old.get_unchecked(x, y);
                let new_cell = new.get_unchecked(x, y);
                if !old_cell.bits_eq(new_cell) {
                    changes.push((x, y));
                }
            }
        }

        Self { changes }
    }

    /// Number of changed cells
    #[inline]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Access raw changes
    pub fn changes(&self) -> &[(u16, u16)] {
        &self.changes
    }

    /// Convert point changes into row-major runs for efficient emission
    pub fn runs(&self, _width: u16) -> Vec<ChangeRun> {
        let mut runs = Vec::new();
        if self.changes.is_empty() {
            return runs;
        }

        // Sort changes row-major
        let mut sorted: Vec<_> = self.changes.iter().copied().collect();
        sorted.sort_by_key(|(x, y)| (*y, *x));

        let mut i = 0;
        while i < sorted.len() {
            let (x0, y) = sorted[i];
            let mut x1 = x0;
            i += 1;

            // Coalesce consecutive x positions
            while i < sorted.len() {
                let (x, yy) = sorted[i];
                if yy != y || x != x1 + 1 {
                    break;
                }
                x1 = x;
                i += 1;
            }

            runs.push(ChangeRun { y, x0, x1 });
        }

        runs
    }
}

/// A contiguous run of changed cells on a single row
pub struct ChangeRun {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
}
```

---

## Chapter 7: GraphemePool and LinkRegistry

### 7.1 GraphemePool: Interned Complex Strings

```rust
/// Pool for complex grapheme clusters (emoji, ZWJ sequences)
/// Reference-counted slots for memory efficiency
pub struct GraphemePool {
    strings: Vec<Option<GraphemeSlot>>,
    lookup: HashMap<String, GraphemeId>,
    free_list: Vec<u32>,
}

struct GraphemeSlot {
    text: String,
    width: u8,
    refcount: u32,
}

impl GraphemePool {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            lookup: HashMap::new(),
            free_list: Vec::new(),
        }
    }

    /// Intern a grapheme cluster, returning existing ID if present
    pub fn intern(&mut self, s: &str) -> GraphemeId {
        // Check if already interned
        if let Some(&id) = self.lookup.get(s) {
            self.incref(id);
            return id;
        }

        // Calculate display width
        let width = unicode_width::UnicodeWidthStr::width(s)
            .min(127) as u8;

        // Find or create slot
        let slot_idx = if let Some(idx) = self.free_list.pop() {
            idx
        } else {
            let idx = self.strings.len() as u32;
            self.strings.push(None);
            idx
        };

        let id = GraphemeId::new(slot_idx, width);
        self.strings[slot_idx as usize] = Some(GraphemeSlot {
            text: s.to_string(),
            width,
            refcount: 1,
        });
        self.lookup.insert(s.to_string(), id);

        id
    }

    /// Get string for ID
    pub fn get(&self, id: GraphemeId) -> Option<&str> {
        self.strings.get(id.slot())
            .and_then(|slot| slot.as_ref())
            .map(|s| s.text.as_str())
    }

    /// Increment refcount
    fn incref(&mut self, id: GraphemeId) {
        if let Some(Some(slot)) = self.strings.get_mut(id.slot()) {
            slot.refcount = slot.refcount.saturating_add(1);
        }
    }

    /// Decrement refcount, freeing if zero
    pub fn decref(&mut self, id: GraphemeId) {
        let slot_idx = id.slot();
        if let Some(Some(slot)) = self.strings.get_mut(slot_idx) {
            slot.refcount = slot.refcount.saturating_sub(1);
            if slot.refcount == 0 {
                self.lookup.remove(&slot.text);
                self.strings[slot_idx] = None;
                self.free_list.push(slot_idx as u32);
            }
        }
    }
}
```

### 7.2 LinkRegistry: Hyperlink Management

```rust
/// Registry for OSC 8 hyperlinks
/// Maps link IDs to URLs for efficient deduplication
pub struct LinkRegistry {
    links: Vec<Option<String>>,
    lookup: HashMap<String, u32>,
    free_list: Vec<u32>,
}

impl LinkRegistry {
    pub fn new() -> Self {
        Self {
            links: Vec::new(),
            lookup: HashMap::new(),
            free_list: Vec::new(),
        }
    }

    /// Register a URL, returning existing ID if present
    pub fn register(&mut self, url: &str) -> u32 {
        if let Some(&id) = self.lookup.get(url) {
            return id;
        }

        let id = if let Some(idx) = self.free_list.pop() {
            idx
        } else {
            let idx = self.links.len() as u32;
            self.links.push(None);
            idx
        };

        self.links[id as usize] = Some(url.to_string());
        self.lookup.insert(url.to_string(), id);
        id
    }

    /// Get URL for ID
    pub fn get(&self, id: u32) -> Option<&str> {
        self.links.get(id as usize)
            .and_then(|slot| slot.as_ref())
            .map(|s| s.as_str())
    }

    /// Unregister a link (for cleanup)
    pub fn unregister(&mut self, id: u32) {
        if let Some(Some(url)) = self.links.get(id as usize) {
            self.lookup.remove(url);
            self.links[id as usize] = None;
            self.free_list.push(id);
        }
    }
}
```

---

## Chapter 8: Presenter (ANSI Writer)

### 8.1 State-Tracked Presenter

```rust
use std::io::{self, Write, BufWriter};

/// Presenter: emits ANSI sequences to transform terminal state
/// Tracks current style/cursor to minimize output
pub struct Presenter<W: Write> {
    writer: BufWriter<W>,
    current_style: Option<CellStyle>,
    current_link: Option<u32>,
    cursor_x: u16,
    cursor_y: u16,
    capabilities: TerminalCapabilities,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    fg: PackedRgba,
    bg: PackedRgba,
    attrs: StyleFlags,
}

impl<W: Write> Presenter<W> {
    pub fn new(writer: W, capabilities: TerminalCapabilities) -> Self {
        Self {
            writer: BufWriter::with_capacity(64 * 1024, writer), // 64KB buffer
            current_style: None,
            current_link: None,
            cursor_x: 0,
            cursor_y: 0,
            capabilities,
        }
    }

    /// Present a frame, diffing against previous
    pub fn present(
        &mut self,
        buffer: &Buffer,
        diff: &BufferDiff,
        pool: &GraphemePool,
        links: &LinkRegistry,
    ) -> io::Result<()> {
        // Start synchronized output if supported
        if self.capabilities.sync_output {
            self.writer.write_all(b"\x1b[?2026h")?;
        }

        // Emit changes using run grouping for efficiency
        self.emit_diff(buffer, diff, pool, links)?;

        // Reset style at end
        self.writer.write_all(b"\x1b[0m")?;
        self.current_style = None;

        // Close any open link
        if self.current_link.is_some() {
            self.writer.write_all(b"\x1b]8;;\x1b\\")?;
            self.current_link = None;
        }

        // End synchronized output
        if self.capabilities.sync_output {
            self.writer.write_all(b"\x1b[?2026l")?;
        }

        self.writer.flush()
    }

    /// Emit diff using row-major runs (NOT per-cell cursor moves)
    fn emit_diff(
        &mut self,
        buffer: &Buffer,
        diff: &BufferDiff,
        pool: &GraphemePool,
        links: &LinkRegistry,
    ) -> io::Result<()> {
        // Convert changes to runs for efficient emission
        for run in diff.runs(buffer.width()) {
            // Single cursor move per run
            self.move_cursor_to(run.x0, run.y)?;

            // Emit cells sequentially (cursor advances naturally)
            for x in run.x0..=run.x1 {
                let cell = buffer.get_unchecked(x, run.y);
                self.emit_cell(cell, pool, links)?;
            }
        }

        Ok(())
    }

    /// Full redraw (no diff)
    pub fn present_full(
        &mut self,
        buffer: &Buffer,
        pool: &GraphemePool,
        links: &LinkRegistry,
    ) -> io::Result<()> {
        if self.capabilities.sync_output {
            self.writer.write_all(b"\x1b[?2026h")?;
        }

        // Move to origin
        self.writer.write_all(b"\x1b[H")?;
        self.cursor_x = 0;
        self.cursor_y = 0;

        // Emit all cells row by row
        for y in 0..buffer.height() {
            for x in 0..buffer.width() {
                let cell = buffer.get_unchecked(x, y);
                self.emit_cell(cell, pool, links)?;
            }
            // Newline at end of row (except last)
            if y + 1 < buffer.height() {
                self.writer.write_all(b"\r\n")?;
                self.cursor_x = 0;
                self.cursor_y += 1;
            }
        }

        self.writer.write_all(b"\x1b[0m")?;
        self.current_style = None;

        if self.current_link.is_some() {
            self.writer.write_all(b"\x1b]8;;\x1b\\")?;
            self.current_link = None;
        }

        if self.capabilities.sync_output {
            self.writer.write_all(b"\x1b[?2026l")?;
        }

        self.writer.flush()
    }

    fn move_cursor_to(&mut self, x: u16, y: u16) -> io::Result<()> {
        if self.cursor_x == x && self.cursor_y == y {
            return Ok(());
        }

        // Emit CUP sequence (1-indexed)
        write!(self.writer, "\x1b[{};{}H", y + 1, x + 1)?;
        self.cursor_x = x;
        self.cursor_y = y;
        Ok(())
    }

    fn emit_cell(
        &mut self,
        cell: &Cell,
        pool: &GraphemePool,
        links: &LinkRegistry,
    ) -> io::Result<()> {
        // Skip continuation cells
        if cell.is_continuation() {
            return Ok(());
        }

        // Emit style changes
        self.emit_style_changes(cell)?;

        // Emit link changes
        self.emit_link_changes(cell, links)?;

        // Emit content
        self.emit_content(cell, pool)?;

        // Advance cursor
        self.cursor_x += cell.width() as u16;

        Ok(())
    }

    fn emit_style_changes(&mut self, cell: &Cell) -> io::Result<()> {
        let new_style = CellStyle {
            fg: cell.fg,
            bg: cell.bg,
            attrs: cell.attrs.flags(),
        };

        if Some(new_style) == self.current_style {
            return Ok(());
        }

        // Reset and apply new style (simplest approach)
        // Could optimize to emit only changed attributes
        self.writer.write_all(b"\x1b[0")?;

        // Attributes
        let attrs = new_style.attrs;
        if attrs.contains(StyleFlags::BOLD) { self.writer.write_all(b";1")?; }
        if attrs.contains(StyleFlags::DIM) { self.writer.write_all(b";2")?; }
        if attrs.contains(StyleFlags::ITALIC) { self.writer.write_all(b";3")?; }
        if attrs.contains(StyleFlags::UNDERLINE) { self.writer.write_all(b";4")?; }
        if attrs.contains(StyleFlags::BLINK) { self.writer.write_all(b";5")?; }
        if attrs.contains(StyleFlags::REVERSE) { self.writer.write_all(b";7")?; }
        if attrs.contains(StyleFlags::STRIKETHROUGH) { self.writer.write_all(b";9")?; }

        // Foreground (true color)
        if new_style.fg.a() > 0 {
            write!(self.writer, ";38;2;{};{};{}",
                   new_style.fg.r(), new_style.fg.g(), new_style.fg.b())?;
        }

        // Background (true color)
        if new_style.bg.a() > 0 {
            write!(self.writer, ";48;2;{};{};{}",
                   new_style.bg.r(), new_style.bg.g(), new_style.bg.b())?;
        }

        self.writer.write_all(b"m")?;
        self.current_style = Some(new_style);

        Ok(())
    }

    fn emit_link_changes(&mut self, cell: &Cell, registry: &LinkRegistry) -> io::Result<()> {
        let new_link = cell.attrs.link_id();
        let current = self.current_link.unwrap_or(0);

        if new_link == current {
            return Ok(());
        }

        if new_link == 0 {
            // Close link
            self.writer.write_all(b"\x1b]8;;\x1b\\")?;
            self.current_link = None;
        } else if let Some(url) = registry.get(new_link) {
            // Open new link
            write!(self.writer, "\x1b]8;;{}\x1b\\", url)?;
            self.current_link = Some(new_link);
        }

        Ok(())
    }

    fn emit_content(&mut self, cell: &Cell, pool: &GraphemePool) -> io::Result<()> {
        if cell.content.0 == 0 {
            // Empty cell = space
            self.writer.write_all(b" ")?;
        } else if cell.content.is_grapheme() {
            // Complex grapheme from pool
            let id = GraphemeId(cell.content.0 & !0x8000_0000);
            if let Some(s) = pool.get(id) {
                self.writer.write_all(s.as_bytes())?;
            } else {
                self.writer.write_all(b"?")?;
            }
        } else {
            // Single char
            let c = char::from_u32(cell.content.0).unwrap_or(' ');
            let mut buf = [0u8; 4];
            self.writer.write_all(c.encode_utf8(&mut buf).as_bytes())?;
        }

        Ok(())
    }
}
```

### 8.2 Optional Dedicated Render/Output Thread (One-Writer Enforcement)

For agent harness workloads, it is common to have:
- background tasks producing log output
- UI updates on ticks
- subprocess output capture

If these can interleave uncontrolled, inline mode breaks. A dedicated output thread is an optional but
high leverage solution.

Design sketch:
- `RenderThread` owns:
  - `TerminalSession` writer handle
  - `Presenter`
  - `prev_buffer`
  - inline-mode policy state (ui rect, scroll margins, etc.)
- The program sends messages:

```rust
enum OutMsg {
    Log(Vec<u8>),             // already encoded UTF-8 bytes
    Render(Buffer),           // desired UI buffer
    Resize { w: u16, h: u16 },
    SetMode(ScreenMode),
    Shutdown,
}
```

Rules:
- All terminal writes happen in the render thread.
- UI presents are coalesced:
  - drop intermediate `Render` messages if a newer one exists
  - keep latest only (like vsync)
- Log writes are newline-aware and can be chunked to avoid starving UI.

Testing:
- PTY tests should run both with and without render thread enabled.

---

## Chapter 9: Terminal Protocol Support

### 9.1 Capability Detection

```rust
/// Terminal capabilities detected from environment
#[derive(Clone, Debug)]
pub struct TerminalCapabilities {
    pub true_color: bool,
    pub colors_256: bool,
    pub sync_output: bool,        // DEC mode 2026
    pub osc8_hyperlinks: bool,
    pub kitty_keyboard: bool,
    pub focus_events: bool,
    pub bracketed_paste: bool,
    pub mouse_sgr: bool,
}

impl TerminalCapabilities {
    /// Detect from environment
    pub fn detect() -> Self {
        // Honor NO_COLOR (de-facto standard) as an explicit "disable color" signal.
        // This should flow into the ColorProfile resolution in ftui-style.
        // NOTE: We still keep true_color capability knowledge, but style resolution may choose mono.
        let no_color = std::env::var("NO_COLOR").is_ok();

        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let term = std::env::var("TERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

        let true_color = !no_color && (colorterm.contains("truecolor")
            || colorterm.contains("24bit")
            || term.contains("24bit")
            || matches!(term_program.as_str(), "iTerm.app" | "WezTerm" | "Alacritty" | "Ghostty"));

        let colors_256 = !no_color && (true_color
            || term.contains("256color")
            || std::env::var("TERM").map(|t| t.contains("256")).unwrap_or(false));

        // Sync output: known support in Kitty, WezTerm, Alacritty, Ghostty
        let sync_output = matches!(term_program.as_str(), "WezTerm" | "Alacritty" | "Ghostty")
            || term.contains("kitty")
            || std::env::var("KITTY_WINDOW_ID").is_ok();

        // OSC 8 hyperlinks: many modern terminals, but not universal.
        // Prefer conservative "off unless likely" heuristics; allow explicit override via config.
        let osc8_hyperlinks = !no_color && (
            matches!(term_program.as_str(), "iTerm.app" | "WezTerm" | "Ghostty")
            || term.contains("kitty")
        );

        Self {
            true_color,
            colors_256,
            sync_output,
            osc8_hyperlinks,
            kitty_keyboard: term.contains("kitty") || std::env::var("KITTY_WINDOW_ID").is_ok(),
            focus_events: true, // Widely supported
            bracketed_paste: true, // Widely supported
            mouse_sgr: true, // Widely supported
        }
    }

    /// Optional runtime probing (future enhancement)
    ///
    /// Environment heuristics are imperfect. For high-confidence behavior, ftui may optionally
    /// perform runtime feature probes (feature-gated, with timeouts), such as:
    /// - device attribute queries (DA) to identify terminal families
    /// - OSC queries for color / hyperlink support
    /// - tmux passthrough negotiation
    ///
    /// This must never block app startup indefinitely; probes must be bounded and skippable.

    /// Minimal fallback (no advanced features)
    pub fn basic() -> Self {
        Self {
            true_color: false,
            colors_256: true,
            sync_output: false,
            osc8_hyperlinks: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: false,
            mouse_sgr: false,
        }
    }
}
```

### 9.2 Terminal Lifecycle (RAII Guards)

```rust
use std::io::{self, Write};
use std::panic;

/// Terminal session with RAII cleanup
pub struct TerminalSession<W: Write> {
    writer: W,
    raw_mode_active: bool,
    alt_screen_active: bool,
    mouse_active: bool,
    bracketed_paste_active: bool,
    capabilities: TerminalCapabilities,
}

impl<W: Write> TerminalSession<W> {
    pub fn new(writer: W) -> io::Result<Self> {
        let caps = TerminalCapabilities::detect();
        let session = Self {
            writer,
            raw_mode_active: false,
            alt_screen_active: false,
            mouse_active: false,
            bracketed_paste_active: false,
            capabilities: caps,
        };

        // Install panic hook for cleanup
        let cleanup_on_panic = || {
            // Best-effort cleanup - ignore errors
            let _ = io::stdout().write_all(b"\x1b[?1049l"); // Exit alt screen
            let _ = io::stdout().write_all(b"\x1b[?25h");   // Show cursor
            let _ = io::stdout().write_all(b"\x1b[0m");     // Reset style
            let _ = io::stdout().write_all(b"\x1b[?1000l"); // Disable mouse
            let _ = io::stdout().write_all(b"\x1b[?2004l"); // Disable bracketed paste
            let _ = io::stdout().flush();
        };

        let prev_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            cleanup_on_panic();
            prev_hook(info);
        }));

        Ok(session)
    }

    pub fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }

    pub fn enter_raw_mode(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // Would use termios here in real implementation
        }
        self.raw_mode_active = true;
        Ok(())
    }

    pub fn enter_alt_screen(&mut self) -> io::Result<()> {
        self.writer.write_all(b"\x1b[?1049h")?;
        self.alt_screen_active = true;
        Ok(())
    }

    pub fn enable_mouse(&mut self) -> io::Result<()> {
        // SGR mouse mode (1006) for precise coordinates
        self.writer.write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
        self.mouse_active = true;
        Ok(())
    }

    pub fn enable_bracketed_paste(&mut self) -> io::Result<()> {
        self.writer.write_all(b"\x1b[?2004h")?;
        self.bracketed_paste_active = true;
        Ok(())
    }

    pub fn enable_focus_events(&mut self) -> io::Result<()> {
        self.writer.write_all(b"\x1b[?1004h")?;
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.mouse_active {
            self.writer.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1006l")?;
        }
        if self.bracketed_paste_active {
            self.writer.write_all(b"\x1b[?2004l")?;
        }
        if self.alt_screen_active {
            self.writer.write_all(b"\x1b[?1049l")?;
        }
        // Reset style and show cursor
        self.writer.write_all(b"\x1b[0m\x1b[?25h")?;
        self.writer.flush()
    }
}

impl<W: Write> Drop for TerminalSession<W> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
```

### 9.3 Screen Modes: Inline-First vs AltScreen (Scrollback-Native)

FrankenTUI supports two output policies. **Inline is default.**

#### Inline mode (default)
Goals:
- preserve native scrollback
- allow an append-only log stream + a stable UI chrome region
- avoid full-screen clears
- maintain a strict cursor contract

##### Inline mode is hard (why ftui is opinionated here)

Inline + logs is where most TUIs become flaky:
- logs write while UI is mid-draw
- cursor gets lost
- scrollback gets wiped by a "helpful" full clear
- tmux/screen complicate cursor save/restore and margins

Therefore ftui treats inline mode as a **system**: output ownership, cursor policy, and (optional)
scroll-region management are part of the design—not afterthoughts.

Key idea:
- The application owns a bounded **UI Region** (typically bottom-anchored).
- Everything else is **Log Region** (append-only text written normally).

Inline mode contract:
1) ftui may move the cursor to draw the UI region.
2) ftui must restore the cursor to the "log cursor" after drawing.
3) ftui must never destroy existing scrollback by clearing the full screen.

##### Three implementation strategies (ftui supports a capability-based hybrid)

**Strategy A: Scroll-region (recommended when reliable)**
- Use DECSTBM (set top/bottom margins) to constrain scrolling to the log region.
- This keeps the UI region pinned while logs scroll above it.
- Benefits:
  - fewer redraws of UI during heavy logging
  - less cursor save/restore thrash
- Risks:
  - behavior varies under tmux/screen and some terminals
  - must reset margins on exit and on mode switches

**Strategy B: Overlay redraw (universal fallback)**
- Before writing logs:
  - temporarily clear/hide UI region
  - write logs normally (so they enter scrollback)
  - redraw UI region
- Benefits:
  - simple, works nearly everywhere
- Costs:
  - more redraw work (but diff+buffer keeps this manageable)

**Strategy C: Hybrid**
- Prefer scroll-region when supported and verified (PTY tests in CI).
- Fall back to overlay redraw when uncertain (tmux, unknown terminal, etc.).

##### Recommended inline present sequence (hybrid-safe)

**Always**:
1) Enter synchronized output if available
2) Save cursor position (prefer robust option; see below)
3) Move cursor to UI region anchor
4) Clear UI region lines only (localized clears)
5) Present UI frame (diffed + run-grouped)
6) Restore cursor position
7) Exit synchronized output

**If using scroll-region**:
- On entering inline mode:
  - set scroll margins to log region: top=1, bottom=term_height-ui_height
  - ensure cursor lives inside the scroll region for log writes
- On exit:
  - reset margins to full screen (`CSI r`)

##### Cursor save/restore strategy (robustness)

Terminals differ in which save/restore sequences they support reliably.
ftui should implement a layered strategy:
- Prefer DEC save/restore (`ESC 7` / `ESC 8`) where reliable
- Fall back to ANSI save/restore (`CSI s` / `CSI u`)
- If neither is reliable (rare), emulate by tracking cursor and explicitly moving back

This is testable via PTY: "after present_ui(), cursor returns to expected position."

##### TerminalWriter responsibilities (the real "kernel for harness UIs")

`TerminalWriter` (or render thread) must:
- serialize `write_log` and `present_ui`
- apply the inline strategy (scroll-region vs overlay)
- enforce cursor policy
- enforce localized clears
- coordinate with synchronized output
- coordinate mode toggles (mouse, paste, focus, alt-screen modals)

This is where "agent harness" reliability is won or lost.

Practical API implication:
- Provide a single **TerminalWriter** that mediates:
  - `write_log(...)` (append-only)
  - `present_ui(frame)`
so UI+log cannot interleave uncontrolled.

#### AltScreen mode (opt-in)
Goals:
- classic full-screen TUI experience
- simplest cursor policy and less interaction with scrollback

AltScreen policy:
- Enter alt screen on start and leave on exit
- Full-screen clears allowed
- Cursor restoration policy is simpler; still enforce cleanup on panic

#### Mixed strategy
Many "agent harness" apps benefit from:
- Inline mode for logs + persistent scrollback
- A temporary alt-screen "modal" for complex interactions (pickers/forms)

This should be supported via:
- `ScreenMode::Inline`
- `ScreenMode::AltScreen`
- `ScreenMode::Inline { allow_modal_alt_screen: bool }`

### 9.4 tmux Detection and Passthrough

```rust
/// Check if running inside tmux
pub fn in_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// Wrap sequence for tmux passthrough
pub fn tmux_wrap(sequence: &str) -> String {
    if in_tmux() {
        // Double escapes for tmux
        let escaped = sequence.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{}\x1b\\", escaped)
    } else {
        sequence.to_string()
    }
}
```

---

## Chapter 10: Input Parser

### 10.1 Event Types

```rust
/// Canonical input event
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize { width: u16, height: u16 },
    Paste(PasteEvent),
    Focus(bool),                 // true = gained, false = lost
    Clipboard(ClipboardEvent),   // optional; may be feature-gated
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteEvent {
    pub text: String,
    pub bracketed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEvent {
    pub content: String,
    pub source: ClipboardSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSource { Osc52, Unknown }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
    pub kind: KeyEventKind,   // Press/Repeat/Release when available; default Press
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind { Press, Repeat, Release }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    BackTab,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
    Null,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Modifiers: u8 {
        const SHIFT   = 0b0000_0001;
        const ALT     = 0b0000_0010;
        const CTRL    = 0b0000_0100;
        const SUPER   = 0b0000_1000;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub x: u16,
    pub y: u16,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
```

### 10.2 Input Parser State Machine

```rust
/// Parser for terminal input sequences
/// Includes DoS protection limits
pub struct InputParser {
    state: ParserState,
    buffer: Vec<u8>,
    paste_buffer: Vec<u8>,
    in_paste: bool,

    // DoS protection
    max_csi_len: usize,
    max_osc_len: usize,
    max_paste_len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi,
    CsiParam,
    Osc,
    Ss3,
}

impl InputParser {
    pub fn new() -> Self {
        Self {
            state: ParserState::Ground,
            buffer: Vec::with_capacity(64),
            paste_buffer: Vec::with_capacity(4096),
            in_paste: false,
            max_csi_len: 256,
            max_osc_len: 4096,
            max_paste_len: 1024 * 1024, // 1MB
        }
    }

    /// Parse bytes, returning events
    pub fn parse(&mut self, input: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();

        for &byte in input {
            if let Some(event) = self.process_byte(byte) {
                events.push(event);
            }
        }

        events
    }

    fn process_byte(&mut self, byte: u8) -> Option<Event> {
        // Bracketed paste handling
        if self.in_paste {
            return self.process_paste_byte(byte);
        }

        match self.state {
            ParserState::Ground => self.ground(byte),
            ParserState::Escape => self.escape(byte),
            ParserState::Csi => self.csi(byte),
            ParserState::CsiParam => self.csi_param(byte),
            ParserState::Osc => self.osc(byte),
            ParserState::Ss3 => self.ss3(byte),
        }
    }

    fn ground(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x1b => {
                self.state = ParserState::Escape;
                None
            }
            0x00 => Some(Event::Key(KeyEvent {
                code: KeyCode::Null,
                modifiers: Modifiers::CTRL,
            })),
            0x01..=0x1a => {
                // Ctrl+A through Ctrl+Z
                let c = (byte + 0x60) as char;
                Some(Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    modifiers: Modifiers::CTRL,
                }))
            }
            0x7f => Some(Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                modifiers: Modifiers::empty(),
            })),
            _ => {
                // UTF-8 or ASCII
                if byte & 0x80 == 0 {
                    Some(Event::Key(KeyEvent {
                        code: KeyCode::Char(byte as char),
                        modifiers: Modifiers::empty(),
                    }))
                } else {
                    // Start of UTF-8 sequence
                    self.buffer.push(byte);
                    None
                }
            }
        }
    }

    fn escape(&mut self, byte: u8) -> Option<Event> {
        self.state = ParserState::Ground;
        match byte {
            b'[' => {
                self.state = ParserState::Csi;
                self.buffer.clear();
                None
            }
            b'O' => {
                self.state = ParserState::Ss3;
                None
            }
            b']' => {
                self.state = ParserState::Osc;
                self.buffer.clear();
                None
            }
            _ => {
                // Alt + key
                Some(Event::Key(KeyEvent {
                    code: KeyCode::Char(byte as char),
                    modifiers: Modifiers::ALT,
                }))
            }
        }
    }

    fn csi(&mut self, byte: u8) -> Option<Event> {
        // DoS protection
        if self.buffer.len() >= self.max_csi_len {
            self.state = ParserState::Ground;
            self.buffer.clear();
            return None;
        }

        self.buffer.push(byte);

        match byte {
            b'0'..=b'9' | b';' | b':' | b'<' | b'=' | b'>' | b'?' => {
                self.state = ParserState::CsiParam;
                None
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'~' => {
                self.state = ParserState::Ground;
                self.parse_csi_sequence()
            }
            _ => {
                self.state = ParserState::Ground;
                self.buffer.clear();
                None
            }
        }
    }

    fn csi_param(&mut self, byte: u8) -> Option<Event> {
        if self.buffer.len() >= self.max_csi_len {
            self.state = ParserState::Ground;
            self.buffer.clear();
            return None;
        }

        self.buffer.push(byte);

        match byte {
            b'0'..=b'9' | b';' | b':' => None,
            b'A'..=b'Z' | b'a'..=b'z' | b'~' | b'M' | b'm' => {
                self.state = ParserState::Ground;
                self.parse_csi_sequence()
            }
            _ => {
                self.state = ParserState::Ground;
                self.buffer.clear();
                None
            }
        }
    }

    fn parse_csi_sequence(&mut self) -> Option<Event> {
        let seq = std::mem::take(&mut self.buffer);

        // Bracketed paste start/end
        if seq == b"200~" {
            self.in_paste = true;
            return None;
        }
        if seq == b"201~" {
            self.in_paste = false;
            let text = String::from_utf8_lossy(&self.paste_buffer).to_string();
            self.paste_buffer.clear();
            return Some(Event::Paste(text));
        }

        // Focus events
        if seq == b"I" {
            return Some(Event::FocusGained);
        }
        if seq == b"O" {
            return Some(Event::FocusLost);
        }

        // SGR mouse (1006)
        if seq.starts_with(b"<") && (seq.ends_with(b"M") || seq.ends_with(b"m")) {
            return self.parse_sgr_mouse(&seq);
        }

        // Arrow keys and special keys
        let last = *seq.last()?;
        let params: Vec<u16> = seq[..seq.len() - 1]
            .split(|&b| b == b';')
            .filter_map(|p| std::str::from_utf8(p).ok()?.parse().ok())
            .collect();

        let modifiers = if params.len() > 1 {
            Self::decode_modifiers(params[1])
        } else {
            Modifiers::empty()
        };

        let code = match last {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            b'~' => match params.first().copied().unwrap_or(0) {
                1 => KeyCode::Home,
                2 => KeyCode::Insert,
                3 => KeyCode::Delete,
                4 => KeyCode::End,
                5 => KeyCode::PageUp,
                6 => KeyCode::PageDown,
                15 => KeyCode::F(5),
                17 => KeyCode::F(6),
                18 => KeyCode::F(7),
                19 => KeyCode::F(8),
                20 => KeyCode::F(9),
                21 => KeyCode::F(10),
                23 => KeyCode::F(11),
                24 => KeyCode::F(12),
                _ => return None,
            },
            _ => return None,
        };

        Some(Event::Key(KeyEvent { code, modifiers }))
    }

    fn parse_sgr_mouse(&mut self, seq: &[u8]) -> Option<Event> {
        let is_release = seq.ends_with(b"m");
        let params_str = std::str::from_utf8(&seq[1..seq.len() - 1]).ok()?;
        let params: Vec<u16> = params_str.split(';').filter_map(|s| s.parse().ok()).collect();

        if params.len() != 3 {
            return None;
        }

        let cb = params[0];
        let x = params[1].saturating_sub(1);
        let y = params[2].saturating_sub(1);

        let modifiers = Modifiers::from_bits_truncate(((cb >> 2) & 0x07) as u8);

        let button = match cb & 0x43 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::Left,
        };

        let kind = if cb & 0x40 != 0 {
            // Scroll
            if cb & 0x01 != 0 {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            }
        } else if cb & 0x20 != 0 {
            MouseEventKind::Drag(button)
        } else if is_release {
            MouseEventKind::Up(button)
        } else {
            MouseEventKind::Down(button)
        };

        Some(Event::Mouse(MouseEvent { kind, x, y, modifiers }))
    }

    fn decode_modifiers(n: u16) -> Modifiers {
        let m = n.saturating_sub(1);
        let mut mods = Modifiers::empty();
        if m & 1 != 0 { mods |= Modifiers::SHIFT; }
        if m & 2 != 0 { mods |= Modifiers::ALT; }
        if m & 4 != 0 { mods |= Modifiers::CTRL; }
        mods
    }

    fn ss3(&mut self, byte: u8) -> Option<Event> {
        self.state = ParserState::Ground;
        let code = match byte {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            b'P' => KeyCode::F(1),
            b'Q' => KeyCode::F(2),
            b'R' => KeyCode::F(3),
            b'S' => KeyCode::F(4),
            _ => return None,
        };
        Some(Event::Key(KeyEvent { code, modifiers: Modifiers::empty() }))
    }

    fn osc(&mut self, byte: u8) -> Option<Event> {
        if self.buffer.len() >= self.max_osc_len {
            self.state = ParserState::Ground;
            self.buffer.clear();
            return None;
        }

        // OSC terminated by BEL (0x07) or ST (ESC \)
        if byte == 0x07 {
            self.state = ParserState::Ground;
            self.buffer.clear();
        } else {
            self.buffer.push(byte);
        }
        None
    }

    fn process_paste_byte(&mut self, byte: u8) -> Option<Event> {
        // Check for paste end sequence
        if self.paste_buffer.ends_with(b"\x1b[201") && byte == b'~' {
            self.in_paste = false;
            // Remove the escape sequence from paste buffer
            let len = self.paste_buffer.len();
            self.paste_buffer.truncate(len - 5);
            let text = String::from_utf8_lossy(&self.paste_buffer).to_string();
            self.paste_buffer.clear();
            return Some(Event::Paste(text));
        }

        // DoS protection
        if self.paste_buffer.len() < self.max_paste_len {
            self.paste_buffer.push(byte);
        }
        None
    }
}
```

### 10.3 Input System Extensions (Optional, Feature-Gated)

The kernel input parser must remain small and robust. Additional capabilities can be layered:
- Kitty keyboard protocol decoding
- Clipboard events (where supported)
- Event coalescing:
  - mouse move floods
  - resize storms

Testing requirements:
- fuzz input parser with random byte streams
- hard bounds on CSI/OSC lengths are enforced
- no panics on malformed sequences

---

## Chapter 11: Components (Widgets Ring)

### 11.1 The Renderable Trait

```rust
/// Trait for components that can render to a buffer
pub trait Renderable {
    /// Measure desired size given constraints
    fn measure(&self, ctx: &RenderContext) -> Measurement;

    /// Render to buffer within given rect
    fn render(&self, ctx: &RenderContext, rect: Rect, buffer: &mut Buffer);
}

/// Size measurement with min/max constraints
#[derive(Debug, Clone, Copy, Default)]
pub struct Measurement {
    pub min_width: u16,
    pub min_height: u16,
    pub max_width: Option<u16>,
    pub max_height: Option<u16>,
}

/// Context for rendering (pools, registries, theme)
pub struct RenderContext {
    pub grapheme_pool: std::cell::RefCell<GraphemePool>,
    pub link_registry: std::cell::RefCell<LinkRegistry>,
    pub theme: Theme,
}

/// v5 architecture note:
/// Renderables are a "widgets ring" concept. The sacred kernel is Frame/Buffer/Diff/Presenter.
/// This trait lives in `ftui-widgets` (or feature-gated module) and depends on kernel primitives.
/// The kernel remains usable without widgets.

// Blanket implementations for composition
impl<T: Renderable> Renderable for &T {
    fn measure(&self, ctx: &RenderContext) -> Measurement { (*self).measure(ctx) }
    fn render(&self, ctx: &RenderContext, rect: Rect, buffer: &mut Buffer) {
        (*self).render(ctx, rect, buffer)
    }
}

impl<T: Renderable> Renderable for Box<T> {
    fn measure(&self, ctx: &RenderContext) -> Measurement { (**self).measure(ctx) }
    fn render(&self, ctx: &RenderContext, rect: Rect, buffer: &mut Buffer) {
        (**self).render(ctx, rect, buffer)
    }
}
```

### 11.2 Rect and Layout Primitives

```rust
/// Rectangle for layout and rendering
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.width &&
        y >= self.y && y < self.y + self.height
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);

        if right > x && bottom > y {
            Self::new(x, y, right - x, bottom - y)
        } else {
            Self::default()
        }
    }

    pub fn inner(&self, margin: Sides) -> Self {
        Self::new(
            self.x + margin.left,
            self.y + margin.top,
            self.width.saturating_sub(margin.left + margin.right),
            self.height.saturating_sub(margin.top + margin.bottom),
        )
    }
}

/// Sides for padding/margin
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sides {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Sides {
    pub fn all(v: u16) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }

    pub fn horizontal(v: u16) -> Self {
        Self { top: 0, right: v, bottom: 0, left: v }
    }

    pub fn vertical(v: u16) -> Self {
        Self { top: v, right: 0, bottom: v, left: 0 }
    }
}

// CSS-like tuple conversions
impl From<u16> for Sides {
    fn from(v: u16) -> Self { Self::all(v) }
}

impl From<(u16, u16)> for Sides {
    fn from((v, h): (u16, u16)) -> Self {
        Self { top: v, right: h, bottom: v, left: h }
    }
}

impl From<(u16, u16, u16, u16)> for Sides {
    fn from((t, r, b, l): (u16, u16, u16, u16)) -> Self {
        Self { top: t, right: r, bottom: b, left: l }
    }
}
```

### 11.3 Panel Component

```rust
/// Bordered container with optional title
pub struct Panel<C: Renderable> {
    content: C,
    border: BorderStyle,
    border_fg: Option<Color>,
    title: Option<String>,
    title_align: HAlign,
    style: Style,
}

impl<C: Renderable> Panel<C> {
    pub fn new(content: C) -> Self {
        Self {
            content,
            border: BorderStyle::Rounded,
            border_fg: None,
            title: None,
            title_align: HAlign::Left,
            style: Style::default(),
        }
    }

    pub fn border(mut self, style: BorderStyle) -> Self {
        self.border = style;
        self
    }

    pub fn border_fg(mut self, color: impl Into<Color>) -> Self {
        self.border_fg = Some(color.into());
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn title_align(mut self, align: HAlign) -> Self {
        self.title_align = align;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl<C: Renderable> Renderable for Panel<C> {
    fn measure(&self, ctx: &RenderContext) -> Measurement {
        let inner = self.content.measure(ctx);
        let border_w = if matches!(self.border, BorderStyle::None) { 0 } else { 2 };
        let padding_w = self.style.padding.horizontal() as usize;

        Measurement {
            minimum: inner.minimum + border_w + padding_w,
            maximum: inner.maximum + border_w + padding_w,
        }
    }

    fn render(&self, ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
        let border = match self.border {
            BorderStyle::None => return self.content.render(ctx, area, buf),
            BorderStyle::Rounded => Border::rounded(),
            BorderStyle::Square => Border::square(),
            BorderStyle::Double => Border::double(),
            BorderStyle::Heavy => Border::heavy(),
            BorderStyle::Ascii => Border::ascii(),
            BorderStyle::Hidden => Border::hidden(),
            BorderStyle::Custom(ref b) => b.clone(),
        };

        let fg = self.border_fg.as_ref()
            .map(|c| c.resolve(&ctx.theme))
            .unwrap_or(ctx.theme.foreground());

        // Draw corners
        buf.set(area.x, area.y, Cell::char(border.top_left).fg(fg));
        buf.set(area.x + area.width - 1, area.y, Cell::char(border.top_right).fg(fg));
        buf.set(area.x, area.y + area.height - 1, Cell::char(border.bottom_left).fg(fg));
        buf.set(area.x + area.width - 1, area.y + area.height - 1, Cell::char(border.bottom_right).fg(fg));

        // Draw edges and optional title
        for x in 1..(area.width - 1) {
            buf.set(area.x + x, area.y, Cell::char(border.top).fg(fg));
            buf.set(area.x + x, area.y + area.height - 1, Cell::char(border.bottom).fg(fg));
        }
        for y in 1..(area.height - 1) {
            buf.set(area.x, area.y + y, Cell::char(border.left).fg(fg));
            buf.set(area.x + area.width - 1, area.y + y, Cell::char(border.right).fg(fg));
        }

        // Title overlay on top edge
        if let Some(ref title) = self.title {
            let title_x = match self.title_align {
                HAlign::Left => area.x + 2,
                HAlign::Center => area.x + (area.width - title.len() as u16) / 2,
                HAlign::Right => area.x + area.width - title.len() as u16 - 2,
            };
            for (i, c) in title.chars().enumerate() {
                buf.set(title_x + i as u16, area.y, Cell::char(c).fg(fg));
            }
        }

        // Render content in inner area
        let inner = area.inner(Sides { top: 1, right: 1, bottom: 1, left: 1 });
        buf.push_scissor(inner);
        self.content.render(ctx, inner, buf);
        buf.pop_scissor();
    }
}
```

### 11.4 Spinner Component

```rust
use std::time::{Duration, Instant};

pub struct Spinner {
    style: SpinnerStyle,
    message: String,
    frame: usize,
    last_tick: Instant,
    fg: Option<Color>,
}

#[derive(Clone)]
pub enum SpinnerStyle {
    Dots,
    Line,
    Braille,
    Bounce,
    Custom(Vec<&'static str>),
}

impl SpinnerStyle {
    fn frames(&self) -> &[&str] {
        match self {
            Self::Dots => &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            Self::Line => &["-", "\\", "|", "/"],
            Self::Braille => &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
            Self::Bounce => &["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"],
            Self::Custom(frames) => frames,
        }
    }

    fn frame_duration(&self) -> Duration {
        match self {
            Self::Dots | Self::Braille => Duration::from_millis(80),
            Self::Line => Duration::from_millis(100),
            Self::Bounce => Duration::from_millis(120),
            Self::Custom(_) => Duration::from_millis(80),
        }
    }
}

impl Spinner {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            style: SpinnerStyle::Dots,
            message: message.into(),
            frame: 0,
            last_tick: Instant::now(),
            fg: None,
        }
    }

    pub fn style(mut self, style: SpinnerStyle) -> Self {
        self.style = style;
        self
    }

    pub fn fg(mut self, color: impl Into<Color>) -> Self {
        self.fg = Some(color.into());
        self
    }

    /// Advance animation frame if enough time has passed
    /// Returns true if frame changed (needs redraw)
    pub fn tick(&mut self) -> bool {
        if self.last_tick.elapsed() >= self.style.frame_duration() {
            self.frame = (self.frame + 1) % self.style.frames().len();
            self.last_tick = Instant::now();
            true
        } else {
            false
        }
    }
}

impl Renderable for Spinner {
    fn measure(&self, _ctx: &RenderContext) -> Measurement {
        let frame_width = 2;
        let msg_width = unicode_width::UnicodeWidthStr::width(self.message.as_str());
        Measurement {
            min_width: (frame_width + 1 + msg_width) as u16,
            min_height: 1,
            max_width: Some((frame_width + 1 + msg_width) as u16),
            max_height: Some(1),
        }
    }

    fn render(&self, ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
        let frame = self.style.frames()[self.frame];
        let fg = self.fg.as_ref()
            .map(|c| c.resolve(&ctx.theme))
            .unwrap_or(ctx.theme.primary());

        // Draw spinner frame
        for (i, c) in frame.chars().enumerate() {
            if area.x + i as u16 >= area.x + area.width { break; }
            buf.set(area.x + i as u16, area.y, Cell::char(c).fg(fg));
        }

        // Draw message
        let msg_x = area.x + 2;
        for (i, c) in self.message.chars().enumerate() {
            let x = msg_x + i as u16;
            if x >= area.x + area.width { break; }
            buf.set(x, area.y, Cell::char(c));
        }
    }
}
```

### 11.5 Progress Bar

```rust
pub struct Progress {
    value: f32,
    label: Option<String>,
    filled_char: char,
    empty_char: char,
    filled_fg: Option<Color>,
    empty_fg: Option<Color>,
    show_percentage: bool,
}

impl Progress {
    pub fn new(value: f32) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            label: None,
            filled_char: '█',
            empty_char: '░',
            filled_fg: None,
            empty_fg: None,
            show_percentage: true,
        }
    }

    pub fn value(mut self, v: f32) -> Self {
        self.value = v.clamp(0.0, 1.0);
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn show_percentage(mut self, show: bool) -> Self {
        self.show_percentage = show;
        self
    }

    pub fn filled_fg(mut self, color: impl Into<Color>) -> Self {
        self.filled_fg = Some(color.into());
        self
    }

    pub fn set(&mut self, value: f32) {
        self.value = value.clamp(0.0, 1.0);
    }
}

impl Renderable for Progress {
    fn measure(&self, _ctx: &RenderContext) -> Measurement {
        let label_w = self.label.as_ref().map(|l| l.len() + 1).unwrap_or(0);
        let pct_w = if self.show_percentage { 5 } else { 0 };
        Measurement {
            min_width: (label_w + 10 + pct_w) as u16,
            min_height: 1,
            max_width: Some((label_w + 50 + pct_w) as u16),
            max_height: Some(1),
        }
    }

    fn render(&self, ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
        let mut x = area.x;

        // Label
        if let Some(ref label) = self.label {
            for c in label.chars() {
                if x >= area.x + area.width { break; }
                buf.set(x, area.y, Cell::char(c));
                x += 1;
            }
            x += 1;
        }

        // Bar
        let pct_width = if self.show_percentage { 5 } else { 0 };
        let bar_width = (area.width as usize).saturating_sub((x - area.x) as usize + pct_width);
        let filled_width = ((bar_width as f32) * self.value) as usize;

        let filled_fg = self.filled_fg.as_ref()
            .map(|c| c.resolve(&ctx.theme))
            .unwrap_or(ctx.theme.primary());
        let empty_fg = self.empty_fg.as_ref()
            .map(|c| c.resolve(&ctx.theme))
            .unwrap_or(ctx.theme.muted());

        for i in 0..bar_width {
            let (ch, fg) = if i < filled_width {
                (self.filled_char, filled_fg)
            } else {
                (self.empty_char, empty_fg)
            };
            buf.set(x, area.y, Cell::char(ch).fg(fg));
            x += 1;
        }

        // Percentage
        if self.show_percentage {
            let pct = format!(" {:>3}%", (self.value * 100.0) as u8);
            for c in pct.chars() {
                if x >= area.x + area.width { break; }
                buf.set(x, area.y, Cell::char(c));
                x += 1;
            }
        }
    }
}
```

---

## Chapter 12: Runtime + Agent Harness API

### 12.1 Runtime Model (Bubbletea/Elm lineage, optional but recommended)

The kernel does not require a runtime. However, a standard runtime dramatically improves
agent-ergonomics and testability.

Core contract:
```rust
pub trait Model {
    type Message: From<Event> + Send + 'static;

    fn init(&mut self) -> Cmd<Self::Message> { Cmd::none() }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message>;

    fn view(&self, frame: &mut Frame);
}

pub enum Cmd<M> {
    None,
    Quit,
    Batch(Vec<Cmd<M>>),
    Sequence(Vec<Cmd<M>>),
    Msg(M),
    Tick(std::time::Duration),
    // Async is feature-gated: threadpool or tokio integration
}

impl<M> Cmd<M> {
    pub fn none() -> Self { Self::None }
    pub fn quit() -> Self { Self::Quit }
    pub fn batch(cmds: Vec<Self>) -> Self { Self::Batch(cmds) }
}
```

Program responsibilities:
- Own terminal lifecycle (raw mode, mouse modes, paste, focus, alt screen if enabled)
- Poll input → Event → Message
- Run update loop, schedule ticks/commands
- Render frames only when dirty (or on tick), with FPS cap if desired
- Enforce screen mode policy (Inline/AltScreen)

### 12.2 Deterministic Simulator (Stop regressions forever)

Provide `ProgramSimulator` that:
- injects a sequence of Events / Messages
- captures produced Frames (or buffer snapshots)
- enables snapshot tests for widgets and full apps
- runs without a real terminal (no flakiness)

```rust
pub struct ProgramSimulator<M: Model> {
    model: M,
    frames: Vec<Buffer>,
}

impl<M: Model> ProgramSimulator<M> {
    pub fn new(model: M) -> Self {
        Self { model, frames: Vec::new() }
    }

    pub fn inject_events(&mut self, events: &[Event]) {
        for event in events {
            let msg = M::Message::from(event.clone());
            self.model.update(msg);
        }
    }

    pub fn capture_frame(&mut self, width: u16, height: u16) -> &Buffer {
        let mut frame = Frame {
            buffer: Buffer::new(width, height),
            hit_grid: None,
            cursor_position: None,
            cursor_visible: true,
        };
        self.model.view(&mut frame);
        self.frames.push(frame.buffer);
        self.frames.last().unwrap()
    }
}
```

### 12.3 Minimal "String View" Adapter (Ergonomics)

Support trivial apps with:
```rust
fn view_string(&self) -> String
```
internally: String → Text/Segments → Frame draw → Presenter.

This keeps the kernel disciplined (Frame remains canonical) while preserving the "easy path."

### 12.4 Agent Harness Reference Implementation

```rust
use std::io::{self, Stdout, Write};

/// High-level coordinator for agent harness UIs
pub struct AgentHarness {
    session: TerminalSession<Stdout>,
    buffer: Buffer,
    prev_buffer: Buffer,
    presenter: Presenter<Stdout>,
    pool: GraphemePool,
    links: LinkRegistry,
    screen_mode: ScreenMode,
}

pub enum ScreenMode {
    Inline { ui_height: u16 },
    AltScreen,
}

impl AgentHarness {
    pub fn new(screen_mode: ScreenMode) -> io::Result<Self> {
        let (width, height) = terminal_size::terminal_size()
            .map(|(w, h)| (w.0, h.0))
            .unwrap_or((80, 24));

        let mut session = TerminalSession::new(io::stdout())?;
        session.enter_raw_mode()?;
        session.enable_mouse()?;
        session.enable_bracketed_paste()?;
        session.enable_focus_events()?;

        if matches!(screen_mode, ScreenMode::AltScreen) {
            session.enter_alt_screen()?;
        }

        let capabilities = session.capabilities().clone();

        Ok(Self {
            session,
            buffer: Buffer::new(width, height),
            prev_buffer: Buffer::new(width, height),
            presenter: Presenter::new(io::stdout(), capabilities),
            pool: GraphemePool::new(),
            links: LinkRegistry::new(),
            screen_mode,
        })
    }

    /// Present a frame (diff-based)
    pub fn present(&mut self) -> io::Result<()> {
        let diff = BufferDiff::compute(&self.prev_buffer, &self.buffer);
        self.presenter.present(&self.buffer, &diff, &self.pool, &self.links)?;
        std::mem::swap(&mut self.buffer, &mut self.prev_buffer);
        Ok(())
    }

    /// Write to log region (inline mode only)
    pub fn write_log(&mut self, text: &str) -> io::Result<()> {
        if let ScreenMode::Inline { ui_height } = self.screen_mode {
            // Save cursor, clear UI, write log, restore UI
            // This is simplified - real impl needs more care
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            write!(handle, "\x1b7")?; // Save cursor
            write!(handle, "\x1b[{};1H", self.buffer.height() - ui_height)?;
            write!(handle, "\x1b[{}M", ui_height)?; // Delete UI lines
            write!(handle, "{}", text)?;
            write!(handle, "\x1b8")?; // Restore cursor
            handle.flush()?;
        }
        Ok(())
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffer
    }

    pub fn grapheme_pool(&mut self) -> &mut GraphemePool {
        &mut self.pool
    }

    pub fn link_registry(&mut self) -> &mut LinkRegistry {
        &mut self.links
    }
}
```

### 12.5 Runtime Ergonomics Checklist

The runtime exists to make the *right* behavior easy:
- owns terminal lifecycle
- owns output serialization (one-writer rule)
- handles resize storms and redraw scheduling
- supports ticks/animations without forcing redraw spam
- keeps present bounded (fps cap + coalescing)

Recommended runtime defaults:
- render on input event
- render on tick only when something is "dirty"
- coalesce multiple events into one update loop iteration when possible
- cap UI redraw rate when logs are extremely high volume (but keep logs flowing)

Event coalescing policy (recommended):
- mouse move: keep latest position per frame
- resize: keep latest size per tick
- key repeat: optionally surface `KeyEventKind::Repeat` if kitty protocol enabled

Bounds policy (DoS resistance):
- CSI/OSC/DCS lengths bounded
- bracketed paste bounded (configurable; default large but finite)
- parser must remain linear-time and panic-free

---

## Chapter 13: Testing, Verification, and CI Gates

### 13.1 Unit + Property Tests (Kernel)

Kernel must be "boringly correct." Enforce:
- Unit tests:
  - Cell packing/eq
  - Buffer bounds + scissor/opacity invariants
  - Grapheme pool refcount correctness
  - Link registry correctness
- Property tests (randomized):
  - Diff correctness: applying changes reproduces target buffer
  - Presenter state tracking never emits malformed sequences
  - Width correctness: rendering never writes beyond frame bounds for random strings

### 13.2 Snapshot Tests (Widgets / Apps)

For widgets and runtime-driven apps:
- Snapshot Frame buffers (grid of Cells) across themes and widths
- Snapshot Segment streams for export pipelines
- Ensure deterministic outputs in simulator mode

### 13.3 PTY Integration Tests (Terminal Reality)

To validate "no flicker / no corruption" claims:
- spawn a PTY, run a minimal ftui app, capture terminal output
- assert:
  - cursor is restored after each present() in Inline mode
  - raw mode is restored on exit
  - cursor visibility restored (show cursor)
  - bracketed paste/mouse modes disabled on exit
  - alt screen exited on exit
- run tests for:
  - normal exit
  - panic during render
  - IO error mid-write

### 13.4 Fuzzing (Input Parser)

Fuzz `InputParser::parse` with random byte streams:
- no panics
- sequence length limits enforce bounds
- parser remains linear time for worst-case inputs

### 13.5 CI Enforcement

CI should enforce:
- clippy + fmt
- unit/property/snapshot/PTY tests
- performance benchmarks as baselines (non-blocking initially, but tracked)
- feature matrix build:
  - default (safe scalar)
  - +simd
  - +extras

### 13.6 Decision Gates (Operational)

- Gate 1: Inline stability demo + PTY tests passing
- Gate 2: Diff/presenter property tests passing
- Gate 3: Unicode width suite passing (ZWJ/emoji/combining)
- Gate 4: Cleanup discipline verified under panic

---

## Chapter 14: Migration Map and Integration Strategy

### 14.1 Migration Principles
- Extract primitives first (Cell/Buffer/Presenter/InputParser)
- Keep adapters for old abstractions short-lived
- Avoid widget ports until the kernel is stable (Gate 1-2)
- Treat "export" as an extra layer using Segment pipeline, not kernel

### 14.2 Source → ftui mapping (conceptual)

**opentui_rust → ftui-render / ftui-core**
- Buffer/Cell/GraphemePool → ftui-render
- Presenter/AnsiWriter → ftui-render
- Input parser + caps → ftui-core
- HitGrid → ftui-render (optional)

**rich_rust → ftui-text / ftui-style / ftui-extras**
- Segment/measurement/text → ftui-text
- Markup parser → ftui-text or ftui-extras (feature gated)
- Live hooks/export adapters → ftui-extras

**charmed_rust → ftui-runtime / ftui-style / ftui-widgets**
- Bubbletea runtime → ftui-runtime (optional but recommended)
- Lipgloss ergonomics → ftui-style (split responsibilities)
- Bubbles/widgets → ftui-widgets

### 14.3 Compatibility Strategy
- Keep terminal protocol support in kernel (caps + presenter)
- Keep UI-specific features (markdown/syntax/forms) in extras
- Maintain a reference "agent harness" app as the integration testbed

---

## Chapter 15: Implementation Roadmap (Phased, No Time Estimates)

### Phase -1: Decision Spikes (tiny experiments, fast learning)

Before building a big structure, run small spikes that de-risk the hardest unknowns:
- [ ] Inline mode spike:
  - implement both scroll-region and overlay strategies in a tiny prototype
  - verify behavior in: plain terminal, tmux, and at least one modern terminal emulator
  - choose a default strategy and document fallback rules (ADR)
- [ ] Presenter validation spike:
  - build terminal-model harness (subset of escape sequences)
  - validate that a present cycle produces expected grid + cursor behavior
- [ ] Backend spike:
  - validate raw mode + resize + input reading across platforms
  - decide default backend

**Exit criteria**: one ADR per spike capturing decision + rationale + consequences.

### Phase 0: Contracts + Workspace Skeleton
- [ ] Establish workspace crate layout (ftui-core/render/style/text/layout/runtime/widgets/extras)
- [ ] Define public contracts for kernel types:
  - `Cell`, `Buffer`, `Frame`, `Presenter`, `TerminalSession`, `Event`
- [ ] Write ADRs for the locked decisions (Section 0.6)
- [ ] Add `ftui` facade crate with a thin stable API surface
- [ ] Add `forbid(unsafe_code)` in all crates except optional `ftui-simd`

**Exit criteria**: public API compiles; minimal demo crate prints a frame in Inline mode.

### Phase 1: Core Render Kernel
- [ ] `Cell` (16 bytes), `CellContent`, `CellAttrs`
- [ ] `PackedRgba` with Porter-Duff blending
- [ ] `GraphemeId`, `GraphemePool` with ref-counting
- [ ] `Buffer` with scissor/opacity stacks
- [ ] `Rect`, `Sides`, `Measurement`
- [ ] Unit tests for all types

**Exit criteria**: unit tests pass; scalar bits_eq is correct; buffer invariants enforced.

### Phase 2: Diff + Presenter (Near-minimal ANSI)
- [ ] `BufferDiff` with SIMD path (feature-gated)
- [ ] `Presenter` with state tracking
- [ ] `TerminalCapabilities` detection
- [ ] Synchronized output (DEC 2026)
- [ ] OSC 8 hyperlinks
- [ ] Panic hook installation
- [ ] Run grouping (row-major ChangeRuns) + style-run coalescing

**Exit criteria**: single-write-per-frame; run grouping works; inline demo shows no flicker on supported terminals.

### Phase 3: Input System
- [ ] Event types (Key, Mouse, Resize, Paste, Focus)
- [ ] `InputParser` state machine
- [ ] SGR mouse protocol
- [ ] Bracketed paste handling
- [ ] DoS protection limits
- [ ] Fuzz harness for parser

**Exit criteria**: parser passes fuzzing and deterministic tests for key/mouse/paste/resize.

### Phase 4: Styling (Split responsibilities)
- [ ] `Style` with bitflags property tracking
- [ ] CSS-like shorthand (Sides from tuples)
- [ ] `Color` enum with profile resolution
- [ ] `Theme` system with presets
- [ ] `Border` presets and custom
- [ ] Markup parser `[bold red]text[/]`
- [ ] Define `CellStyle` vs higher-level style resolution

**Exit criteria**: deterministic style merge + theme resolution; markup is correct under tests.

### Phase 5: Layout + Components (Widgets ring)
- [ ] `Renderable` trait
- [ ] `Panel` with borders and titles
- [ ] `Spinner` with multiple styles
- [ ] `Progress` bar
- [ ] `Text` with wrapping
- [ ] `Table` (basic)
- [ ] `Viewport` and `TextInput` (agent harness essentials)

**Exit criteria**: snapshot tests for widgets pass; hit testing works where applicable.

### Phase 6: Runtime + Agent Harness Reference App
- [ ] Implement `Program` + `Model` + `Cmd` + scheduler (ftui-runtime)
- [ ] Implement `ProgramSimulator`
- [ ] Build `ftui-harness` reference app:
  - inline scrollback log stream + UI region
  - tool indicators, status line, input area
  - streaming render updates
  - no flicker, no corruption

**Exit criteria**: harness demo passes PTY tests; inline mode is stable under sustained output.

### Phase 7: Extras + Polish
- [ ] Documentation (rustdoc)
- [ ] Examples gallery
- [ ] Performance audit
- [ ] Terminal compatibility testing
- [ ] CI/CD setup
- [ ] Export adapters (Segment/HTML/SVG) feature-gated
- [ ] Optional syntax highlighting/markdown feature-gated

**Exit criteria**: stable kernel API; docs + examples; performance baselines recorded; compatibility matrix validated.

---

## Chapter 16: Terminal Compatibility Matrix

| Feature | Kitty | WezTerm | Alacritty | Ghostty | iTerm2 | GNOME Term | Win Term |
|---------|-------|---------|-----------|---------|--------|------------|----------|
| True Color | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Sync Output | ✓ | ✓ | ✓ | ✓ | ✗ | ✗ | ✗ |
| OSC 8 Links | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| Kitty Keyboard | ✓ | ✓ | ✗ | ✓ | ✗ | ✗ | ✗ |
| Kitty Graphics | ✓ | ✓ | ✗ | ✓ | ✗ | ✗ | ✗ |
| Sixel | ✗ | ✓ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Focus Events | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Bracketed Paste | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

Inline mode notes:
- Inline mode works everywhere but depends on a correct cursor-save/restore strategy.
- Sync output is a best-effort optimization where supported; inline correctness must not depend on it.

---

## Chapter 17: Operational Playbook (How This Actually Ships)

This chapter exists to prevent the common failure mode of kernel projects:
infinite refinement without a stable deliverable.

### 17.1 "Definition of Done" gates become merge gates

- Any PR that touches:
  - inline mode cursor policy
  - presenter output sequencing
  - input parser
  - width measurement
must include:
- tests (unit/property/pty/snapshot as appropriate)
- a note on which invariants it preserves

### 17.2 ADR discipline (how decisions get locked)

Rules:
- Every "this might be controversial later" design choice gets an ADR.
- ADRs are short and opinionated:
  - Context
  - Decision
  - Alternatives considered
  - Consequences
  - Test plan / how we'll know it worked

### 17.3 Minimal deliverables order (avoid the widget trap)

Ship in this order:
1) Kernel: buffer/diff/presenter + terminal session + inline stability
2) Runtime: update/view loop + simulator + snapshot testing
3) Harness: reference app that proves the inline/log/UI story
4) Widgets: only the ones needed by harness (viewport/input/status/progress)
5) Extras: markdown/syntax/export (feature-gated)

### 17.4 What "agent-ergonomic" means in practice

The API should make it easy to do the right thing:
- streaming logs that never break the UI
- stable UI chrome that never flickers
- input handling that doesn't miss paste/focus/resize
- strong defaults + escape hatches

Concrete checklist:
- "Hello world harness" in <200 LOC
- "Tool output streaming" is one function call
- "Pinned bottom UI" is one config option
- "Temporary full-screen modal" is supported without rewriting the app

### 17.5 Immediate Next Steps (first PRs)

1) Create workspace + `ftui` facade crate (empty re-exports, docs scaffold)
2) Implement `TerminalSession` RAII with hard cleanup rules + PTY test
3) Implement `Buffer`/`Cell`/`Frame` + unit tests
4) Implement `Presenter` scalar path + terminal-model test harness
5) Implement inline mode `TerminalWriter` with overlay redraw strategy
6) Build the smallest `ftui-harness` demo that:
   - writes logs continuously
   - redraws pinned UI region
   - survives resize
   - exits cleanly on panic (forced)

---

## Appendix A: ANSI Escape Sequence Reference

### SGR (Select Graphic Rendition)
```
\x1b[0m         Reset all
\x1b[1m         Bold
\x1b[2m         Dim
\x1b[3m         Italic
\x1b[4m         Underline
\x1b[5m         Blink
\x1b[7m         Reverse
\x1b[9m         Strikethrough
\x1b[38;2;R;G;Bm  Foreground true color
\x1b[48;2;R;G;Bm  Background true color
\x1b[38;5;Nm      Foreground 256 color
\x1b[48;5;Nm      Background 256 color
```

### Cursor Control
```
\x1b[H          Home (1,1)
\x1b[{r};{c}H   Move to row r, col c (1-indexed)
\x1b[{n}A       Up n
\x1b[{n}B       Down n
\x1b[{n}C       Forward n
\x1b[{n}D       Back n
\x1b[s          Save position (ANSI)
\x1b[u          Restore position (ANSI)
\x1b7           Save position (DEC)
\x1b8           Restore position (DEC)
```

### Screen Control
```
\x1b[?1049h     Enter alt screen
\x1b[?1049l     Exit alt screen
\x1b[2J         Clear screen
\x1b[K          Clear to end of line
\x1b[?25h       Show cursor
\x1b[?25l       Hide cursor
```

### Synchronized Output (DEC 2026)
```
\x1b[?2026h     Begin sync
\x1b[?2026l     End sync
```

### OSC 8 Hyperlinks
```
\x1b]8;;URL\x1b\\    Start link
\x1b]8;;\x1b\\       End link
```

### Mouse Modes
```
\x1b[?1000h     Enable button tracking
\x1b[?1002h     Enable button+motion
\x1b[?1006h     Enable SGR encoding
\x1b[?1000l     Disable mouse
```

### Bracketed Paste
```
\x1b[?2004h     Enable
\x1b[?2004l     Disable
\x1b[200~       Paste start
\x1b[201~       Paste end
```

### Focus Events
```
\x1b[?1004h     Enable
\x1b[?1004l     Disable
\x1b[I          Focus gained
\x1b[O          Focus lost
```

---

## Appendix B: Glossary

| Term | Definition |
|------|------------|
| **Cell** | Single grid position (content + fg + bg + attrs) |
| **Buffer** | 2D array of Cells representing display state |
| **Frame** | Buffer + metadata for a render pass |
| **Diff** | Set of (x, y) positions that changed between buffers |
| **Presenter** | State-tracked ANSI emitter |
| **Grapheme** | User-perceived character (may be multiple codepoints) |
| **GraphemePool** | Interned storage for complex grapheme clusters |
| **LinkRegistry** | URL storage for OSC 8 hyperlinks |
| **Scissor** | Clipping rectangle for rendering |
| **SGR** | Select Graphic Rendition (style codes) |
| **OSC** | Operating System Command (escape sequence) |
| **CSI** | Control Sequence Introducer (`\x1b[`) |
| **DEC** | Digital Equipment Corporation (terminal standard) |
| **ZWJ** | Zero Width Joiner (connects graphemes into compound) |
| **Porter-Duff** | Compositing algebra for alpha blending |

---

## Appendix C: Risk Register (Top Risks + Mitigations)

1) **Inline mode cursor corruption**
   - Mitigation: PTY tests + strict cursor policy + centralized writer API for logs/UI.

2) **Unicode width bugs**
   - Mitigation: curated test corpus + snapshot tests + avoid "byte length == width" except proven ASCII fast path.

3) **Terminal capability mismatches**
   - Mitigation: conservative defaults + feature detection + robust fallbacks; never assume sync output.

4) **Unsafe creep**
   - Mitigation: feature-gated simd module, documented invariants, safe default always available.

5) **Presenter byte bloat**
   - Mitigation: run grouping + style-run coalescing + output-length benchmarks in CI.

6) **Interleaved stdout writes from user code**
   - Mitigation: provide a `TerminalSession`/`TerminalWriter` API and recommend exclusive ownership patterns.

---

## Appendix D: Extracted Library Implementations

This appendix contains detailed code extractions from the three source libraries that form the basis of FrankenTUI's design.

### D.1 From rich_rust: Segment System

The Segment is the atomic unit of styled text. Key insight: use `Cow<str>` to avoid allocation when borrowing.

```rust
/// Atomic rendering unit - styled text slice
/// From rich_rust/src/segment.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<'a> {
    pub text: Cow<'a, str>,              // Borrowed when possible
    pub style: Option<Style>,
    pub control: Option<SmallVec<[ControlCode; 2]>>,  // Stack-allocated for 0-2 codes
}

/// Control codes for cursor/screen manipulation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ControlType {
    Bell = 1,
    CarriageReturn = 2,
    Home = 3,
    Clear = 4,
    ShowCursor = 5,
    HideCursor = 6,
    EnableAltScreen = 7,
    DisableAltScreen = 8,
    CursorUp = 9,
    CursorDown = 10,
    CursorForward = 11,
    CursorBack = 12,
    CursorNextLine = 13,
    CursorPrevLine = 14,
    EraseLine = 15,
    SetTitle = 16,
}

impl<'a> Segment<'a> {
    /// Split segment at cell position (not byte position!)
    /// Critical for correct text wrapping with wide chars
    pub fn split_at_cell(&self, cell_pos: usize) -> (Self, Self) {
        if self.is_control() {
            return (self.clone(), Self::default());
        }

        let mut width = 0;
        let mut byte_pos = 0;

        for (i, c) in self.text.char_indices() {
            let char_width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if width + char_width > cell_pos {
                break;
            }
            width += char_width;
            byte_pos = i + c.len_utf8();
        }

        // Optimized split using Cow to avoid unnecessary allocation
        let (left, right) = match &self.text {
            Cow::Borrowed(s) => {
                let (l, r) = s.split_at(byte_pos);
                (Cow::Borrowed(l), Cow::Borrowed(r))
            }
            Cow::Owned(s) => {
                let (l, r) = s.split_at(byte_pos);
                (Cow::Owned(l.to_string()), Cow::Owned(r.to_string()))
            }
        };

        (
            Self::new(left, self.style.clone()),
            Self::new(right, self.style.clone()),
        )
    }

    /// Cell width (display width, not byte length)
    pub fn cell_length(&self) -> usize {
        if self.is_control() { return 0; }
        unicode_width::UnicodeWidthStr::width(self.text.as_ref())
    }
}
```

### D.2 From rich_rust: Markup Parser

Regex-based parser for Rich-style markup like `[bold red]text[/]`.

```rust
use std::sync::LazyLock;
use regex::Regex;

/// Markup tag pattern: [style] or [/style] or [/]
static TAG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\\*)\[([A-Za-z#/@][^\[\]]*?)\]").expect("invalid regex")
});

/// Parsed tag from markup
#[derive(Debug, Clone)]
pub struct Tag {
    pub name: String,
    pub parameters: Option<String>,
}

impl Tag {
    pub fn is_closing(&self) -> bool {
        self.name.starts_with('/')
    }

    pub fn base_name(&self) -> &str {
        self.name.trim_start_matches('/')
    }
}

/// Parse markup with style stack for nesting
/// Handles escape sequences: \\[ becomes literal [
pub fn parse_markup<'a>(markup: &'a str, base_style: Style) -> Vec<Segment<'a>> {
    // Fast path: no markup tags
    if !markup.contains('[') {
        return vec![Segment::new(Cow::Borrowed(markup), Some(base_style))];
    }

    let mut segments = Vec::new();
    let mut style_stack: Vec<(usize, Style)> = vec![(0, base_style.clone())];
    let mut current_text = String::new();
    let mut last_end = 0;

    for cap in TAG_PATTERN.captures_iter(markup) {
        let full_match = cap.get(0).unwrap();
        let backslashes = cap.get(1).map_or("", |m| m.as_str());
        let tag_content = cap.get(2).map_or("", |m| m.as_str());
        let match_start = full_match.start();

        // Text before this match
        if match_start > last_end {
            current_text.push_str(&markup[last_end..match_start]);
        }

        // Handle escape sequences
        let num_backslashes = backslashes.len();
        let escaped = num_backslashes % 2 == 1;

        // Add literal backslashes (pairs become singles)
        if num_backslashes > 0 {
            current_text.push_str(&"\\".repeat(num_backslashes / 2));
        }

        if escaped {
            // Escaped bracket: literal text
            current_text.push('[');
            current_text.push_str(tag_content);
            current_text.push(']');
        } else {
            // Emit accumulated text with current style
            if !current_text.is_empty() {
                let style = style_stack.last().map(|(_, s)| s.clone());
                segments.push(Segment::new(
                    Cow::Owned(std::mem::take(&mut current_text)),
                    style,
                ));
            }

            // Process tag
            let tag = parse_tag(tag_content);
            if tag.is_closing() {
                // Pop matching style from stack
                if style_stack.len() > 1 {
                    style_stack.pop();
                }
            } else {
                // Opening tag: push new style
                let new_style = parse_style_tag(&tag, style_stack.last().map(|(_, s)| s));
                style_stack.push((segments.len(), new_style));
            }
        }

        last_end = full_match.end();
    }

    // Remaining text
    if last_end < markup.len() {
        current_text.push_str(&markup[last_end..]);
    }
    if !current_text.is_empty() {
        let style = style_stack.last().map(|(_, s)| s.clone());
        segments.push(Segment::new(Cow::Owned(current_text), style));
    }

    segments
}
```

### D.3 From rich_rust: Measurement Protocol

```rust
/// Measurement for layout negotiation
/// Tracks minimum (tightest) and maximum (ideal) widths
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextMeasurement {
    pub minimum: usize,  // Minimum cells required (tightest compression)
    pub maximum: usize,  // Maximum cells required (ideal unconstrained)
}

impl TextMeasurement {
    pub const ZERO: Self = Self { minimum: 0, maximum: 0 };

    /// Union: take max of both bounds (for side-by-side layout)
    pub fn union(self, other: Self) -> Self {
        Self {
            minimum: self.minimum.max(other.minimum),
            maximum: self.maximum.max(other.maximum),
        }
    }

    /// Stack: add both bounds (for vertical stacking)
    pub fn stack(self, other: Self) -> Self {
        Self {
            minimum: self.minimum + other.minimum,
            maximum: self.maximum + other.maximum,
        }
    }

    /// Clamp to constraints
    pub fn clamp(self, min_width: Option<usize>, max_width: Option<usize>) -> Self {
        let mut result = self;
        if let Some(min_w) = min_width {
            result.minimum = result.minimum.max(min_w);
            result.maximum = result.maximum.max(min_w);
        }
        if let Some(max_w) = max_width {
            result.minimum = result.minimum.min(max_w);
            result.maximum = result.maximum.min(max_w);
        }
        result
    }
}
```

### D.4 From charmed_rust: Style with Bitflags Property Tracking

```rust
use bitflags::bitflags;

bitflags! {
    /// Tracks which properties have been explicitly set
    /// Enables proper inheritance: unset properties inherit from parent
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Props: u64 {
        const BOLD = 1 << 0;
        const ITALIC = 1 << 1;
        const UNDERLINE = 1 << 2;
        const STRIKETHROUGH = 1 << 3;
        const BLINK = 1 << 4;
        const REVERSE = 1 << 5;
        const DIM = 1 << 6;
        const HIDDEN = 1 << 7;

        const FG_COLOR = 1 << 8;
        const BG_COLOR = 1 << 9;

        const WIDTH = 1 << 10;
        const HEIGHT = 1 << 11;
        const MAX_WIDTH = 1 << 12;
        const MAX_HEIGHT = 1 << 13;

        const PADDING_TOP = 1 << 14;
        const PADDING_RIGHT = 1 << 15;
        const PADDING_BOTTOM = 1 << 16;
        const PADDING_LEFT = 1 << 17;

        const MARGIN_TOP = 1 << 18;
        const MARGIN_RIGHT = 1 << 19;
        const MARGIN_BOTTOM = 1 << 20;
        const MARGIN_LEFT = 1 << 21;

        const BORDER_STYLE = 1 << 22;
        const BORDER_TOP = 1 << 23;
        const BORDER_RIGHT = 1 << 24;
        const BORDER_BOTTOM = 1 << 25;
        const BORDER_LEFT = 1 << 26;

        const ALIGN_H = 1 << 27;
        const ALIGN_V = 1 << 28;

        const LINK = 1 << 29;
        const TAB_WIDTH = 1 << 30;
    }
}

/// Full style with all properties and explicit tracking
#[derive(Clone, Default)]
pub struct FullStyle {
    props: Props,              // Which properties are explicitly set
    attrs: StyleFlags,         // Boolean attributes
    fg_color: Option<Color>,
    bg_color: Option<Color>,
    width: u16,
    height: u16,
    max_width: u16,
    max_height: u16,
    padding: Sides<u16>,
    margin: Sides<u16>,
    border_style: BorderStyle,
    align_h: HAlign,
    align_v: VAlign,
    link: Option<String>,
}

impl FullStyle {
    pub fn new() -> Self { Self::default() }

    // Fluent builders that track which properties are set

    pub fn fg(mut self, color: impl Into<Color>) -> Self {
        self.fg_color = Some(color.into());
        self.props |= Props::FG_COLOR;
        self
    }

    pub fn bg(mut self, color: impl Into<Color>) -> Self {
        self.bg_color = Some(color.into());
        self.props |= Props::BG_COLOR;
        self
    }

    pub fn bold(mut self) -> Self {
        self.attrs |= StyleFlags::BOLD;
        self.props |= Props::BOLD;
        self
    }

    pub fn italic(mut self) -> Self {
        self.attrs |= StyleFlags::ITALIC;
        self.props |= Props::ITALIC;
        self
    }

    pub fn padding<E: Into<Sides<u16>>>(mut self, edges: E) -> Self {
        self.padding = edges.into();
        self.props |= Props::PADDING_TOP | Props::PADDING_RIGHT |
                      Props::PADDING_BOTTOM | Props::PADDING_LEFT;
        self
    }

    /// Inherit unset properties from parent
    pub fn inherit(mut self, parent: &FullStyle) -> Self {
        let unset = !self.props;

        if unset.contains(Props::FG_COLOR) {
            self.fg_color = parent.fg_color.clone();
        }
        if unset.contains(Props::BG_COLOR) {
            self.bg_color = parent.bg_color.clone();
        }
        // Merge attributes for unset flags
        self.attrs |= parent.attrs & StyleFlags::from_bits_truncate(unset.bits() as u8);

        self
    }
}
```

### D.5 From charmed_rust: Border System

```rust
/// Complete border definition with all corners and edges
#[derive(Clone, Debug)]
pub struct Border {
    pub top: char,
    pub bottom: char,
    pub left: char,
    pub right: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    // For tables with internal dividers
    pub middle_left: char,
    pub middle_right: char,
    pub middle: char,
    pub middle_top: char,
    pub middle_bottom: char,
}

impl Border {
    pub fn rounded() -> Self {
        Self {
            top: '─', bottom: '─', left: '│', right: '│',
            top_left: '╭', top_right: '╮',
            bottom_left: '╰', bottom_right: '╯',
            middle_left: '├', middle_right: '┤',
            middle: '┼', middle_top: '┬', middle_bottom: '┴',
        }
    }

    pub fn square() -> Self {
        Self {
            top: '─', bottom: '─', left: '│', right: '│',
            top_left: '┌', top_right: '┐',
            bottom_left: '└', bottom_right: '┘',
            middle_left: '├', middle_right: '┤',
            middle: '┼', middle_top: '┬', middle_bottom: '┴',
        }
    }

    pub fn double() -> Self {
        Self {
            top: '═', bottom: '═', left: '║', right: '║',
            top_left: '╔', top_right: '╗',
            bottom_left: '╚', bottom_right: '╝',
            middle_left: '╠', middle_right: '╣',
            middle: '╬', middle_top: '╦', middle_bottom: '╩',
        }
    }

    pub fn heavy() -> Self {
        Self {
            top: '━', bottom: '━', left: '┃', right: '┃',
            top_left: '┏', top_right: '┓',
            bottom_left: '┗', bottom_right: '┛',
            middle_left: '┣', middle_right: '┫',
            middle: '╋', middle_top: '┳', middle_bottom: '┻',
        }
    }

    pub fn ascii() -> Self {
        Self {
            top: '-', bottom: '-', left: '|', right: '|',
            top_left: '+', top_right: '+',
            bottom_left: '+', bottom_right: '+',
            middle_left: '+', middle_right: '+',
            middle: '+', middle_top: '+', middle_bottom: '+',
        }
    }

    pub fn hidden() -> Self {
        Self {
            top: ' ', bottom: ' ', left: ' ', right: ' ',
            top_left: ' ', top_right: ' ',
            bottom_left: ' ', bottom_right: ' ',
            middle_left: ' ', middle_right: ' ',
            middle: ' ', middle_top: ' ', middle_bottom: ' ',
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum BorderStyle {
    #[default]
    None,
    Rounded,
    Square,
    Double,
    Heavy,
    Ascii,
    Hidden,
    Custom(Border),
}
```

### D.6 From charmed_rust: Color System with Profile Detection

```rust
/// Color profile (terminal capability level)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorProfile {
    Ascii,      // No color (1-bit)
    Ansi,       // 16 colors (4-bit)
    Ansi256,    // 256 colors (8-bit)
    TrueColor,  // 16 million colors (24-bit)
}

impl ColorProfile {
    /// Detect from environment
    pub fn detect() -> Self {
        // Check NO_COLOR first (standard for disabling color)
        if std::env::var("NO_COLOR").is_ok() {
            return Self::Ascii;
        }

        // Check COLORTERM for true color
        if let Ok(colorterm) = std::env::var("COLORTERM") {
            if colorterm == "truecolor" || colorterm == "24bit" {
                return Self::TrueColor;
            }
        }

        // Check TERM
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("kitty") || term.contains("wezterm") ||
               term.contains("alacritty") || term.contains("ghostty") {
                return Self::TrueColor;
            }
            if term.contains("256color") {
                return Self::Ansi256;
            }
            if term.contains("color") || term.starts_with("xterm") {
                return Self::Ansi;
            }
            if term == "dumb" {
                return Self::Ascii;
            }
        }

        // Conservative default
        Self::TrueColor
    }
}

/// Color that can be resolved against a profile
#[derive(Clone, Debug)]
pub enum AdaptiveColor {
    /// Direct RGB value
    Rgb(u8, u8, u8),
    /// ANSI color index (0-255)
    Ansi(u8),
    /// Named color (resolved from theme)
    Named(String),
    /// Adaptive color (different for light/dark)
    Adaptive { light: Box<AdaptiveColor>, dark: Box<AdaptiveColor> },
}

impl AdaptiveColor {
    /// Parse from string: "#RGB", "#RRGGBB", "red", "123" (ANSI)
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();

        // Hex color
        if s.starts_with('#') {
            let hex = &s[1..];
            return match hex.len() {
                3 => {
                    let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                    let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                    let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                    Some(Self::Rgb(r, g, b))
                }
                6 => {
                    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                    Some(Self::Rgb(r, g, b))
                }
                _ => None,
            };
        }

        // ANSI index
        if let Ok(n) = s.parse::<u8>() {
            return Some(Self::Ansi(n));
        }

        // Named color
        Some(Self::Named(s.to_string()))
    }
}

/// Convert RGB to 256-color palette index
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Check grayscale first (more accurate for grays)
    if r == g && g == b {
        if r < 8 { return 16; }
        if r > 248 { return 231; }
        return 232 + ((r - 8) / 10).min(23);
    }

    // 6×6×6 color cube
    let r6 = (r as u16 * 6 / 256) as u8;
    let g6 = (g as u16 * 6 / 256) as u8;
    let b6 = (b as u16 * 6 / 256) as u8;
    16 + 36 * r6 + 6 * g6 + b6
}

/// Convert RGB to 16-color ANSI index
fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    let brightness = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    let bright = brightness > 127;

    let base = match (r > 127, g > 127, b > 127) {
        (false, false, false) => 0,
        (true, false, false) => 1,
        (false, true, false) => 2,
        (true, true, false) => 3,
        (false, false, true) => 4,
        (true, false, true) => 5,
        (false, true, true) => 6,
        (true, true, true) => 7,
    };

    if bright { base + 8 } else { base }
}
```

---

## Appendix E: Performance Benchmarks

### E.1 Target Metrics

| Operation | Target | Justification |
|-----------|--------|---------------|
| Cell comparison | < 1ns | Bitwise u128 compare |
| Buffer diff (80×24) | < 500µs | Sequential scan, SIMD |
| Frame render | < 1ms | Required for 60 FPS |
| Input parse | < 20µs/event | State machine |
| Style parse | < 8µs (cached) | From rich_rust benchmarks |
| Color blend | < 10ns | Single Porter-Duff op |
| Grapheme intern (hit) | < 100ns | Hash lookup |
| Grapheme intern (miss) | < 1µs | Hash + alloc + width |

### E.2 Benchmark Framework

```rust
#[cfg(test)]
mod benchmarks {
    use criterion::{black_box, criterion_group, criterion_main, Criterion};

    fn bench_cell_comparison(c: &mut Criterion) {
        let cell_a = Cell::new('A', Style::default());
        let cell_b = Cell::new('B', Style::default());

        c.bench_function("cell_bits_eq", |b| {
            b.iter(|| black_box(cell_a.bits_eq(&cell_b)))
        });
    }

    fn bench_buffer_diff(c: &mut Criterion) {
        let buf1 = Buffer::new(80, 24);
        let mut buf2 = Buffer::new(80, 24);

        // 5% cells changed
        for i in 0..96 {
            buf2.cells[i * 20] = Cell::new('X', Style::default());
        }

        c.bench_function("buffer_diff_5pct", |b| {
            b.iter(|| black_box(BufferDiff::compute(&buf1, &buf2)))
        });
    }

    fn bench_grapheme_pool(c: &mut Criterion) {
        let mut pool = GraphemePool::new();

        // Pre-populate
        pool.intern("hello");
        pool.intern("world");
        pool.intern("👨‍👩‍👧");

        c.bench_function("grapheme_intern_hit", |b| {
            b.iter(|| black_box(pool.intern("hello")))
        });

        c.bench_function("grapheme_intern_miss", |b| {
            let mut i = 0;
            b.iter(|| {
                let s = format!("new_grapheme_{}", i);
                i += 1;
                black_box(pool.intern(&s))
            })
        });
    }

    fn bench_presenter_emit(c: &mut Criterion) {
        let buf = Buffer::new(80, 24);
        let pool = GraphemePool::new();
        let links = LinkRegistry::new();
        let caps = TerminalCapabilities::detect();

        c.bench_function("presenter_full_frame", |b| {
            b.iter(|| {
                let mut output = Vec::with_capacity(64 * 1024);
                let mut presenter = Presenter::new(&mut output, caps.clone());
                black_box(presenter.present_full(&buf, &pool, &links))
            })
        });
    }

    criterion_group!(benches,
        bench_cell_comparison,
        bench_buffer_diff,
        bench_grapheme_pool,
        bench_presenter_emit
    );
    criterion_main!(benches);
}
```

---

## Conclusion

FrankenTUI v5.0 represents an "ultimate hybrid" synthesis of three excellent terminal UI libraries:

1. **From opentui_rust**: Cache-optimal 16-byte cells, bitwise comparison, Porter-Duff blending, grapheme pooling, scissor/opacity stacks, cell-level diffing

2. **From rich_rust**: Cow<str> segments for zero-copy, regex-based markup parser, event-driven span rendering, LRU width caching, measurement protocol

3. **From charmed_rust**: Bitflags property tracking for inheritance, CSS-like tuple shorthand, adaptive colors, border presets, color profile detection

The result is a **scrollback-native, zero-flicker, agent-ergonomic** terminal UI library plan that is:
- mathematically grounded (cell+diff model)
- operationally realistic (inline cursor policies, run grouping, fallbacks)
- test-driven (property + snapshot + PTY + fuzz)
- layered for long-term maintainability (kernel/widgets/extras)

Performance targets:
- **< 1ns** cell comparison (bitwise)
- **< 500µs** frame diff (80×24)
- **< 1ms** total frame time
- **16 bytes** per cell (4 cells per cache line)
- **Zero heap allocation** for 99% of cells

*FrankenTUI: Where rigor meets practical ergonomics.*
