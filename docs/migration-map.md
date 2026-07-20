# Migration Map

> How three Rust codebases became FrankenTUI, and what to know before contributing.

---

## 1. Migration Principles

FrankenTUI is **not a port**. It synthesizes ideas from three reference codebases into a
new system with its own invariants, types, and architecture. The reference code lives in
`legacy_reference_library_code/` as read-only consultation material.

### What "migration" means here

- **Keep concepts, not types.** A good idea (like grapheme pooling or state-tracked
  presentation) survives; the original struct layout does not.
- **Re-derive from invariants.** Every ftui type exists because a kernel invariant
  requires it, not because the source library had it.
- **No backwards compatibility.** There is no upgrade path from any source library to
  ftui. Shared names (e.g., `Cell`, `Buffer`, `Style`) are coincidental; the semantics
  are ftui's own.
- **Smaller is better.** If a source feature does not serve the kernel invariants or
  the agent-harness use case, it is dropped. It can be added later behind a feature
  gate if demand materializes.

---

## 2. Source Libraries

| Library | Repository | Role in ftui |
|---------|------------|-------------|
| **opentui_rust** | `legacy_reference_library_code/opentui_rust/` | Terminal I/O, cell/buffer/diff/presenter, grapheme pool, link registry, input parsing, ANSI output |
| **rich_rust** | `legacy_reference_library_code/rich_rust/` | Text and styling: segments, spans, colors, themes, text measurement, console output, box drawing |
| **charmed_rust** | `legacy_reference_library_code/charmed_rust/` | Elm/Bubbletea runtime: Program/Model/Cmd, event system, tick scheduling, keyboard/mouse handling |

Each library contributed **concepts**, not code. The ftui implementations are written from
scratch against ftui's own constraints (16-byte cells, one-writer rule, forbid unsafe, etc.).

---

## 3. Conceptual Mapping: Source to ftui

### Terminal and rendering (from opentui_rust)

| opentui_rust | ftui | Notes |
|-------------|------|-------|
| `Cell` (variable size) | `Cell` (16 bytes, `#[repr(C, align(16))]`) | Redesigned for cache-line alignment. 4 cells per 64-byte line. |
| `GraphemePool` | `GraphemePool` in `ftui-render` | Same concept (intern complex graphemes, reference by ID). ftui packs slot+width into 32 bits. |
| `LinkRegistry` | `LinkRegistry` in `ftui-render` | Same concept (OSC 8 hyperlink tracking). ftui uses 16-bit IDs packed into `CellAttrs`. |
| `Buffer` (2D grid) | `Buffer` in `ftui-render` | Row-major `Vec<Cell>`. ftui adds scissor and opacity stacks. |
| `renderer/` module | `Presenter` in `ftui-render` | State-tracked ANSI emitter. ftui tracks SGR, cursor, and OSC 8 link state to minimize output. |
| `buffer/diff.rs` | `BufferDiff` in `ftui-render` | Row-major scan with run coalescing. ftui adds branchless `bits_eq` for the hot path. |
| `terminal/` module | `TerminalSession` + `TerminalWriter` in `ftui-core`/`ftui-runtime` | Split into RAII lifecycle guard and serialized output gate. |
| `ansi/` module | `ftui-core` input parser + `Presenter` output | Input and output ANSI handling are separated by design. |
| `input/` module | `InputParser` in `ftui-core` | Canonical `Event` types. ftui adds bounded parsing with DoS limits. |
| `highlight/` module | Planned in `ftui-extras` (feature-gated) | Deferred to Phase 5. Not kernel. |
| `color.rs` (56 KB) | `PackedRgba` (4 bytes) + `Color` enum in `ftui-style` | Dramatically simplified. One packed type for the renderer; richer types in the style layer. |
| `style.rs` | Split: `CellAttrs` (renderer) + `Style` (user-facing) | CellAttrs is 4 bytes in the cell. Style has Option-wrapped fields for CSS-like cascading. |

### Text and styling (from rich_rust)

| rich_rust | ftui | Notes |
|----------|------|-------|
| `Segment` (styled text unit) | `Segment` in `ftui-text` | `Cow<str>` + optional `Style`. Same concept, ftui-specific Style type. |
| `Text` (multi-line styled) | `Text` in `ftui-text` | Collection of `Line`s. ftui adds explicit wrap/truncate modes. |
| `style.rs` (57 KB) | `Style` in `ftui-style` (compact) | ftui uses Option-wrapped fields for inheritance. `merge()` cascades child over parent. |
| `color.rs` (53 KB) | `Color` enum + canonical `ColorDepth` downgrade pipeline | ftui provides TrueColor -> 256 -> 16 -> Mono. `PackedRgba` handles the renderer path. |
| `theme.rs` (38 KB) | `Theme` + `ResolvedTheme` in `ftui-style` | Semantic color slots. Builder pattern. Themes resolve to concrete colors at render time. |
| `console.rs` (112 KB) | Not ported | ftui has no "console" abstraction. Output is always through `TerminalWriter`. |
| `measure.rs` | Width cache in `ftui-text` | LRU cache for display width measurements. Default 1000 entries. |
| `box.rs` (box drawing) | `Block` + `Borders` in `ftui-widgets` | Simplified. Border styles are an enum, not arbitrary character sets. |
| `live.rs` (live display) | `ScreenMode::Inline` in `ftui-runtime` | ftui's inline mode replaces the "live display" concept entirely. |
| `logging.rs` | `LogSink` in `ftui-runtime` | Routed through one-writer. No direct stdout access. |
| `interactive.rs` | `Program`/`Model`/`Cmd` runtime | Replaced by the Elm-like architecture from charmed_rust. |
| `emoji.rs`, `filesize.rs` | Not ported | Utilities. Can be added to extras if needed. |

### Runtime (from charmed_rust)

| charmed_rust | ftui | Notes |
|-------------|------|-------|
| `Program` (Bubbletea-style) | `Program` in `ftui-runtime` | Elm-like loop: init -> update -> view. ftui adds `Cmd::Batch`, `Cmd::Sequence`. |
| `Model` trait | `Model` trait in `ftui-runtime` | Same concept. `update()` returns `Cmd<Message>`, `view()` renders to `Frame`. |
| `Cmd` (side effects) | `Cmd` enum in `ftui-runtime` | Extended: None, Quit, Batch, Sequence, Msg, Tick, Log, Task. |
| Event types (Key, Mouse) | `Event` enum in `ftui-core` | Unified and normalized. Supports multiple keyboard protocols. |
| Tick scheduling | Built into `Program` in `ftui-runtime` | Configurable tick rate, integrated with the update loop. |
| `App` builder | `App`/`AppBuilder` in `ftui-runtime` | Ergonomic setup with sensible defaults. |
| Async runtime integration | Optional, not required | ftui supports sync path. Async is feature-gated if needed. |

---

## 4. What Survives vs. What Gets Dropped

### Survives (concepts that ftui preserves)

| Concept | Source | Why it survives |
|---------|--------|----------------|
| Cell-based diff rendering | opentui | Fundamental to flicker-free terminal output |
| Grapheme pooling | opentui | Required for correct wide/complex character handling in 16-byte cells |
| State-tracked presentation | opentui | Minimizes ANSI output volume; prevents dangling style state |
| Hyperlink registry (OSC 8) | opentui | Links are first-class in agent harness UIs |
| Elm-like Model/update/view | charmed | Clean separation of state, logic, and rendering |
| Side-effect commands (Cmd) | charmed | Testable, deterministic side effects |
| Styled text spans | rich | Text needs inline styling for any useful UI |
| Color downgrade pipeline | rich | Terminal color support varies; graceful fallback is required |
| Theme/semantic colors | rich | Users should not hard-code RGB values |
| Width measurement caching | rich | Text measurement is expensive; caching is necessary |
| RAII terminal cleanup | opentui | Terminals must be restored even on panic |

### Dropped (deliberately excluded from ftui)

| Feature | Source | Why dropped |
|---------|--------|-------------|
| Direct console abstraction | rich | Violates one-writer rule. All output through `TerminalWriter`. |
| Monolithic widget library | rich | "Widget trap" risk. ftui ships harness-essential widgets only in v1. |
| Backwards-compatible API surface | all | Fresh design from invariants. No upgrade path. |
| Full terminal emulation | opentui | ftui targets output correctness for supported sequences, not a VT100 emulator. |
| Multiple simultaneous terminals | all | Violates one-writer rule by design. |
| Complex async-only runtime | charmed | Sync path must remain viable. Async is optional. |
| Advanced layout (named grid areas) | rich | Flexbox is sufficient for v1 harness UIs. |
| Per-cell string storage | opentui | Would break 16-byte cell invariant. GraphemePool indirection instead. |
| Column-major storage | n/a | Terminal rendering is inherently row-oriented. |
| Emoji database / filesize utilities | rich | Application-level concerns, not a UI kernel's job. |

### De-scoped to Extras (Phase 5, feature-gated)

These features exist conceptually in source libraries but are explicitly not v1 kernel:

| Feature | Gate | Phase |
|---------|------|-------|
| Markdown renderer | `ftui-extras` | 5 |
| Syntax highlighting | `ftui-extras` | 5 |
| Canvas / image protocols | `ftui-extras` | 5 |
| Forms system | `ftui-extras` | 5 |
| SSH integration | `ftui-extras` | 5 |
| HTML/SVG export | `ftui-extras` | 5 |

---

## 5. Compatibility Strategy

### With source libraries

There is no compatibility. ftui shares no public API surface with any source library.
If you previously used opentui_rust, rich_rust, or charmed_rust, you are starting fresh
with ftui's types.

### With the Rust TUI ecosystem

ftui is not a fork of ratatui, crossterm, or tui-rs. It uses crossterm as a dependency
for raw mode, input, and resize handling (locked via [ADR-003](adr/ADR-003-terminal-backend.md)),
but the rendering pipeline, buffer system, and runtime are ftui's own.

### With terminals

ftui targets modern terminals that support ANSI/VT100 sequences. The compatibility
matrix is documented in [docs/reference/terminal-compatibility.md](reference/terminal-compatibility.md).

Key decisions locked via ADRs:
- [ADR-001](adr/ADR-001-inline-mode.md): Inline mode uses hybrid strategy (overlay redraw baseline + scroll-region optimization)
- [ADR-002](adr/ADR-002-presenter-emission.md): Presenter uses reset+apply SGR strategy for v1
- [ADR-004](adr/ADR-004-windows-v1-scope.md): Windows v1 targets ConPTY only (no legacy ConHost)
- [ADR-005](adr/ADR-005-one-writer-rule.md): One-writer rule is mandatory
- [ADR-006](adr/ADR-006-untrusted-output-policy.md): Untrusted output is sanitized by default

---

## 6. The Three-Ring Architecture

```
Ring 1 (Kernel)          Ring 2 (Middleware)           Ring 3 (Extras)
+-----------------+      +---------------------+      +------------------+
| ftui-core       |<-----| ftui-style          |      | ftui-extras      |
| ftui-render     |<-----| ftui-text           |      | ftui-harness     |
|                 |<-----| ftui-layout         |      | ftui-pty         |
|                 |<-----| ftui-runtime        |      | ftui-simd        |
|                 |<-----| ftui-widgets        |      |                  |
|                 |      | ftui (facade)       |      |                  |
+-----------------+      +---------------------+      +------------------+
 Must be stable.          Built on kernel.              Feature-gated.
 Minimal types.           Reusable, optional.           Never required.
 Correctness-verified.    Evolves faster.               Zero cost if off.
```

**Dependency rule**: Arrows point inward only. Ring 2 depends on Ring 1. Ring 3 depends
on Ring 1 or Ring 2. Ring 1 never depends outward.

**Critical constraint**: `ftui-render` does NOT depend on `ftui-style`. Instead,
`ftui-style` depends on `ftui-render`. Rendering operates on bare `Cell` types,
not style wrappers. This keeps the kernel independent of the styling model.

---

## 7. When to Add a New Crate vs. Extend an Existing One

Add a new crate when **all** of these apply:

1. **Cross-cutting concern.** The functionality affects or is used by multiple existing
   crates.
2. **Clear layering boundary.** The new crate depends strictly downward (no cycles). You
   can draw its dependency edges without introducing a loop.
3. **Feature-gateable.** Disabling the crate does not break any crate in a lower ring.
4. **Independent testability.** The crate has its own unit tests, property tests, or
   benchmarks that validate its invariants in isolation.
5. **Single responsibility.** One clear purpose that you can state in a sentence (e.g.,
   "layout constraints and flex solver," not "UI stuff").

Extend an existing crate when:

- The new functionality is a natural extension of the crate's existing responsibility.
- Adding it does not pull in new external dependencies.
- The crate's public API grows by a small, cohesive set of types.

**Examples:**

| Decision | Rationale |
|----------|-----------|
| `ftui-simd` is a separate crate | Isolated `unsafe`, needs explicit auditing, optional with safe fallback, depends only downward on `ftui-render` |
| `ftui-pty` is a separate crate | Test-only utilities, not a user dependency, keeps test infra out of core |
| `GraphemePool` lives in `ftui-render` | Tightly coupled to `Cell` and `Buffer`; would create a cycle if extracted |
| `Style` lives in `ftui-style` (not `ftui-render`) | Style is user-facing API; renderer uses the packed `CellAttrs`/`PackedRgba` directly |
| `LogSink` lives in `ftui-runtime` | Requires `TerminalWriter` integration; natural part of the runtime's output path |

---

## 8. For Future Contributors

### Before adding a feature

1. Check the [Operational Playbook](operational-playbook.md) for shipping order. If your
   feature is Phase 5 (Extras), it must be feature-gated and cannot touch the kernel.
2. Check the ADRs. If your change contradicts an accepted ADR, you need a new superseding
   ADR, not a silent change.
3. Check whether the source libraries had the feature. If they did and ftui dropped it,
   there is probably a reason in Section 4 above.

### Before adding a dependency

- Ring 1 crates (`ftui-core`, `ftui-render`) have a near-zero dependency budget. Any new
  dependency must be justified by a clear need that cannot be met with std or a few lines
  of code.
- Ring 2 crates can use well-maintained crates (`unicode-width`, `unicode-segmentation`,
  `proptest` for dev) but should avoid large frameworks.
- Ring 3 crates have more freedom but still must not increase build time for users who
  disable the feature gate.

### Before copying code from source libraries

Do not copy code. Read the source for ideas, then implement from ftui's invariants.
The source libraries have different constraints (different cell sizes, different
threading models, different error handling). Copying creates subtle bugs where the
assumptions do not match.

---

## See Also

- [OpenTUI Import — Operations](opentui-import-operations.md) -- operator runbooks, playbooks, and troubleshooting for running migrations with the `doctor_frankentui` CLI
- [Operational Playbook](operational-playbook.md) -- shipping order and quality gates
- [Glossary](glossary.md) -- terminology definitions
- [Cache and Layout Rationale](spec/cache-and-layout.md) -- why 16-byte cells and row-major storage
- [State Machines Spec](spec/state-machines.md) -- presenter and input parser invariants
- [One-Writer Rule](one-writer-rule.md) -- terminal output serialization guidance
