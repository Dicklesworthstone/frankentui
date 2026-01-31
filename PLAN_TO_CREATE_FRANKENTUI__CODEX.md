# PLAN_TO_CREATE_FRANKENTUI__CODEX.md

## FrankenTUI (ftui): The Optimal Terminal UI Kernel
Version 6.3 — Kernel-First, Math-Informed, Practice-Proven Architecture

Note: Code blocks in this plan are design sketches for API shape and invariants.
They are intentionally close to Rust, but not guaranteed to compile verbatim.
The correctness contract lives in invariants, tests, and ADRs.

---

# PART 0: EXECUTIVE BLUEPRINT

## 0.1 Executive Summary
FrankenTUI (ftui) fuses rich_rust, charmed_rust, and opentui_rust into a single
coherent kernel. This is not a port. It is a new kernel optimized for:
- scrollback-native inline mode
- flicker-free output by construction
- deterministic rendering and testing
- performance via cache-local buffers and pooled graphemes

## 0.2 Primary Targets
- Agent harness UIs: log stream + stable UI chrome, native scrollback, no flicker.
- Classic TUIs: dashboards, pickers, editors, forms.
- Export/replay: capture frames or segments for HTML/SVG/text and tests.

## 0.3 Non-Negotiables (Engineering Contract)
- One writer owns the terminal. All terminal-mutating bytes go through ftui.
- Diffed + buffered UI only (no ad-hoc println in render path).
- Inline-first is real; AltScreen is explicit opt-in.
- Safe-by-default: forbid unsafe in core crates.
- Deterministic rendering: same state + size + theme == same frame.
- Cleanup is guaranteed even on panic (raw mode, cursor, alt screen, paste, mouse).

## 0.4 Non-Goals
- Backwards compatibility with upstream APIs.
- A monolithic crate that bundles everything.
- Proving global minimal ANSI output for all cases; we target correctness + near-minimal.

## 0.5 Three-Ring Architecture
1) Kernel (sacred): frame/buffer/cell, diff, presenter, input, modes.
2) Widgets: reusable components on kernel primitives.
3) Extras: markdown, syntax, forms, SSH, export.

## 0.6 Workspace Layout (Crates)
0) ftui (facade)
   - stable public API re-exports (App, Event, Frame, Style, ScreenMode)
   - prelude and defaults

1) ftui-core
   - terminal lifecycle (RAII guards)
   - capability detection
   - input parsing into Event
   - screen mode policies

2) ftui-render
   - Cell/Buffer/Frame
   - GraphemePool, LinkRegistry, HitGrid
   - diff engine + Presenter
   - optional threaded renderer

3) ftui-style
   - Style/Theme
   - color downgrade
   - deterministic style merge

4) ftui-text
   - Text/Span/Segment
   - wrapping/truncation/alignment
   - width caches

5) ftui-layout
   - Rect, constraints, row/col/grid
   - min/max measurement protocol

6) ftui-runtime
   - Program, Model, Cmd, scheduler/ticks
   - deterministic simulator

7) ftui-widgets (feature gated)
8) ftui-extras (feature gated)
9) ftui-harness (examples)
10) ftui-simd (optional, unsafe isolated)
11) ftui-pty (test-only helpers)

## 0.7 Kernel Invariants (Must Always Hold)
- All UI rendering is diffed and buffered.
- Inline mode never clears the full screen.
- Cursor restored after each present per policy.
- Style/link state never left dangling after present/exit.
- Grapheme width is correct for all glyphs (ZWJ, emoji, combining marks).
- Input parsing is lossless for supported sequences and bounded for malformed input.
- Cleanup guaranteed on normal exit and panic.
- Single-writer rule is enforced (inline correctness depends on it).
- Unsafe is isolated in ftui-simd only.

## 0.8 ADRs to Lock Early (Decision Records)
1) Frame is the canonical render target.
2) Inline-first; AltScreen explicit opt-in.
3) Presenter emits row-major runs (not per-cell moves).
4) Style split: CellStyle (renderer-facing) vs Theme/semantic (higher-level).
5) Terminal backend choice for v1 (cross-platform).
6) Inline mode implementation strategy (scroll region vs overlay redraw vs hybrid).
7) Output/concurrency model (single-thread or optional render thread).
8) Span precedence rules (later wins, masks override).
9) Presenter style emission (full reset vs incremental SGR).
10) Windows support scope for v1.

## 0.9 Public API Surface (Target)
### Canonical entrypoint
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
        // draw into frame
    }
}

fn main() -> Result<()> {
    App::new(State { count: 0 })
        .screen_mode(ScreenMode::Inline { ui_height: 6 })
        .run()
}
```

### Easy adapter
- `view_string()` routes String -> Text -> Frame -> Diff -> Presenter.

### Agent harness primitives
- `write_log(text)` (scrollback-native)
- `present_ui(frame)` (atomic, flicker-resistant)
- optional modal AltScreen
- PTY capture for child processes (feature gated)

## 0.10 Output + Concurrency Architecture
### One-Writer Rule
All terminal-mutating bytes are serialized through a single owner.

### Supported modes
- Mode A (default): single-threaded writer.
- Mode B (optional): dedicated render/output thread.

### Atomic present contract
Presenter emits diff as a single buffered write with optional sync output.

### Output mux/capture (harness realism)
- PTY capture for subprocess output
- LogSink / Writer API to avoid raw println

## 0.11 Definition of Done (v1)
- Inline mode stable with streaming logs + UI chrome.
- Diff/presenter correctness validated by terminal-model tests.
- Unicode width correctness proven by corpus tests.
- Style merge semantics deterministic and documented.
- Runtime supports update/view, ticks, batch/sequence, simulator.
- Core harness widgets: viewport/log viewer, status line, input, spinner.
- Docs: harness tutorial, inline vs alt screen, IO ownership guidance.

---

# PART I: FOUNDATIONS

## 1) First Principles
A terminal is a state machine; rendering is the inverse function.
Practical optimality comes from:
- cell-level diffing
- state tracking (cursor/style/link)
- cache-local data structures
- pooled complex content (graphemes/links)

## 2) Canonical Type Definitions (Rust-ish spec)
### Event
```
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize { width: u16, height: u16 },
    Paste(PasteEvent),
    Focus(bool),
    Clipboard(ClipboardEvent),
}
```

### Frame/Cell
```
#[repr(C, align(16))]
pub struct Cell {
    content: CellContent,  // 4 bytes
    fg: PackedRgba,        // 4 bytes
    bg: PackedRgba,        // 4 bytes
    attrs: CellAttrs,      // 4 bytes
}
```

### Style
```
pub struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    attrs: Attrs,
    mask: Attrs,
    link_id: Option<u32>,
    meta: Option<Vec<u8>>,
}
```

### Text
```
pub struct Text { plain: String, spans: Vec<Span>, ... }
```

## 3) Cache and Memory Layout
- 16-byte cell layout for cache efficiency.
- 4 cells per 64-byte cache line.
- 80x24 grid fits in L1 cache.
- Row-major scans for diff.

## 4) Rendering Engine
### Diff
- bits_eq cell comparison
- row-major run grouping
- scratch buffer reuse

### Presenter
- tracks cursor/style/link state
- buffered single write per frame
- sync output if supported (DEC 2026)

## 5) Input System
- bounded CSI/DCS lengths
- bracketed paste with max size
- optional coalescing of noisy events

## 6) Terminal Capabilities + Lifecycle
- detect truecolor, 256, sync output, OSC 8, focus, paste
- RAII guard ensures cleanup on panic

## 7) Style System
- deterministic merges via explicit masks
- theme stack with semantic slots
- colors downgraded by capability

## 8) Text + Layout
- spans resolved deterministically (later wins)
- measurement protocol (min/max)
- layout: row/col/grid

## 9) Runtime Model
```
trait Model {
    type Message: From<Event> + Send + 'static;
    fn init(&self) -> Option<Cmd<Self::Message>> { None }
    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message>;
    fn view(&self, frame: &mut Frame);
}
```

## 10) Inline Mode vs AltScreen
- Inline default with bounded UI region and cursor restore.
- AltScreen opt-in for full-screen apps.

---

# PART II: DECISIONS EXPANDED (REQUESTED)

## 11) Inline Mode Strategy ADR (scroll region vs overlay redraw vs hybrid)

### 11.1 The problem
We need stable UI chrome + scrollback-native logs. Inline mode must preserve
scrollback while keeping the UI region stable and flicker-free.

### 11.2 Options
A) Scroll-region anchoring (DECSTBM)
- Set a scroll region for logs; UI pinned outside scroll region.
- Pros: true pinned UI without redraw; smooth logs.
- Cons: terminal support is uneven; scroll region interaction with cursor save/restore is tricky.

B) Overlay redraw (save/restore cursor)
- Logs append normally; UI is redrawn over a bounded region on each frame.
- Pros: simple, portable; no dependency on scroll region support.
- Cons: must clear UI region each frame; risk of flicker if not buffered.

C) Hybrid
- Use scroll-region if supported; fallback to overlay redraw otherwise.
- Pros: best of both worlds.
- Cons: more complexity and testing.

### 11.3 Decision (proposed)
- Use Hybrid: overlay redraw as baseline, scroll-region as optional optimization.
- Adopt capability flag `caps.scroll_region` to enable DECSTBM path.
- Ensure the API does not expose this complexity; it is an internal policy.

### 11.4 Inline mode policy (baseline overlay redraw)
- UI region anchored at bottom by default (configurable top).
- Render sequence:
  1) Save cursor
  2) Move to UI anchor
  3) Clear UI region lines
  4) Present UI frame
  5) Restore cursor

### 11.5 Tests
- PTY test: log streaming + UI refresh does not corrupt scrollback.
- Cursor policy test: cursor restored after every present.

## 12) Presenter Emission Strategy ADR (SGR reset vs incremental)

### 12.1 Options
A) Always reset (SGR 0) before applying style
- Pros: simplest, correctness guaranteed.
- Cons: more bytes.

B) Incremental SGR diffs
- Pros: fewer bytes, more efficient.
- Cons: complexity; must carefully track attr state.

C) Hybrid
- Reset when style diff is complex; incremental when diff is small.
- Pros: balances simplicity and output size.
- Cons: more code, needs benchmarks.

### 12.2 Decision (proposed)
- Start with A (reset + apply) for correctness and simplicity.
- Add incremental path behind `presenter.incremental_sgr` feature.
- Benchmark real workloads (harness logs + UI chrome) to justify switch.

### 12.3 Benchmarks to run
- 80x24, 120x40, 200x60 UI with logs
- measure total bytes emitted and frame latency

## 13) Terminal Backend Selection + Windows v1 Scope ADR

### 13.1 Backend options
- Crossterm: widely used, cross-platform, robust.
- Termion: Unix-only, lighter but no Windows.
- Custom termios: more control, but requires unsafe.

### 13.2 Decision (proposed)
- Use Crossterm as v1 backend (cross-platform).
- Expose backend trait later if needed, but do not over-abstract in v1.

### 13.3 Windows v1 scope
- Supported: raw mode, key input, basic mouse, 16/256/truecolor where available.
- Best-effort: OSC 8 hyperlinks, sync output, bracketed paste.
- Document gaps clearly.

---

# PART III: EXECUTION PLAN

## 14) Performance Budgets
- 120x40 diff present < 1 ms
- input parse + dispatch < 100 us/event
- wrap 200 lines < 2 ms

## 15) Testing Strategy
- unit tests for diff/width/style
- property tests for diff correctness
- snapshot tests for widgets/themes
- PTY tests for cleanup and cursor correctness
- perf tests for budgets

## 16) Quality Gates (Stop-Ship)
- Gate 1: Inline mode stability
- Gate 2: Diff/presenter correctness (terminal-model)
- Gate 3: Unicode width correctness
- Gate 4: Cleanup correctness on panic

## 17) Migration Map
- rich_rust -> ftui-text/ftui-style/ftui-widgets
- charmed_rust -> ftui-runtime/ftui-style/ftui-widgets
- opentui_rust -> ftui-render/ftui-core

## 18) Phased Implementation Plan
Phase 0: contracts + facade
Phase 1: render kernel
Phase 2: input + terminal
Phase 3: style + text
Phase 4: runtime + simulator
Phase 5: layout + widgets
Phase 6: extras + export
Phase 7: stabilization

---

# PART IV: SUPER DETAILED TODO LIST

The list below is the full task inventory. Do not delete items; check off when done.

## A) ADRs + Decisions
- [ ] ADR-001: Inline mode strategy (hybrid scroll-region + overlay redraw)
  - [ ] Define detection logic for scroll-region capability
  - [ ] Specify fallback rules
  - [ ] Document cursor policy
- [ ] ADR-002: Presenter style emission (reset vs incremental vs hybrid)
  - [ ] Define baseline (reset + apply)
  - [ ] Define incremental feature flag
  - [ ] Benchmark thresholds to enable incremental by default
- [ ] ADR-003: Terminal backend choice (Crossterm v1)
  - [ ] Document rationale
  - [ ] Document future backend trait possibility
- [ ] ADR-004: Windows v1 scope
  - [ ] Enumerate supported/unsupported terminal features
  - [ ] Add doc section for known gaps
- [ ] ADR-005: One-writer rule enforcement
  - [ ] API design to route logs through ftui
  - [ ] Document undefined behavior if violated

## B) Core Types + Contracts
- [ ] Define Cell layout + PackedRgba + CellAttrs
- [ ] Define GraphemeId encoding and GraphemePool API
- [ ] Define LinkRegistry
- [ ] Define Frame + Buffer + HitGrid interfaces
- [ ] Define Event/Key/Mouse types (canonical)
- [ ] Define Style + Theme + ThemeStack
- [ ] Define Text + Span + Segment
- [ ] Define Model + Cmd + Program

## C) Rendering Engine
- [ ] Implement Buffer (flat Vec, scissor, opacity)
  - [ ] Set/get cell with bounds check
  - [ ] Continuation cell handling for wide glyphs
- [ ] Implement diff engine
  - [ ] bits_eq cell compare
  - [ ] row-major scan
  - [ ] run grouping
- [ ] Implement Presenter
  - [ ] cursor tracking
  - [ ] style tracking
  - [ ] link tracking
  - [ ] buffered write
  - [ ] sync output support

## D) Inline Mode
- [ ] Implement overlay redraw policy
  - [ ] cursor save/restore
  - [ ] clear UI region lines
  - [ ] bounded anchor
- [ ] Implement scroll-region path (optional)
  - [ ] DECSTBM setup
  - [ ] compatibility fallbacks
- [ ] Tests for inline correctness

## E) Input + Terminal
- [ ] Input parser with bounded CSI/DCS
- [ ] Bracketed paste support
- [ ] Focus + resize events
- [ ] Capability detection (TERM/COLORTERM/TERM_PROGRAM)
- [ ] Terminal RAII guard with panic hook

## F) Style + Text
- [ ] Style merge algorithm with explicit masks
- [ ] Color downgrade (truecolor -> 256 -> 16 -> mono)
- [ ] Theme stack + semantic slots
- [ ] Span overlap resolution
- [ ] Text wrapping + truncation

## G) Runtime + Scheduler
- [ ] Model/Program loop
- [ ] Cmd batch + sequence
- [ ] Tick scheduling
- [ ] Deterministic simulator

## H) Widgets (v1)
- [ ] Viewport/log viewer
- [ ] Status line / panel
- [ ] Text input
- [ ] Progress + spinner
- [ ] Table and list

## I) Extras
- [ ] Markdown renderer
- [ ] Syntax highlighting
- [ ] Forms
- [ ] Export (HTML/SVG)

## J) Testing + QA
- [ ] Diff property tests
- [ ] Unicode width corpus tests
- [ ] Snapshot tests for widgets
- [ ] PTY cleanup tests
- [ ] Perf benchmarks with budgets

## K) Docs
- [ ] Agent harness tutorial
- [ ] Inline vs alt-screen explanation
- [ ] One-writer rule guidance
- [ ] Windows v1 limitations

