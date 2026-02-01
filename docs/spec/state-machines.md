# State Machines: Terminal + Rendering Pipeline

This document is the formal-ish specification backbone for FrankenTUI.

It is intentionally written to be directly useful for:
- implementation structure (which module owns what)
- invariant placement (type-level vs runtime checks)
- test strategy (unit/property/PTY)

See Bead: bd-10i.13.1.

---

## 1) Terminal State Machine

### 1.1 State Variables
We model the terminal as a state machine that consumes bytes and updates a display grid.

Minimal state (conceptual):
- `cursor`: (x, y)
- `style`: current SGR state (fg/bg/attrs)
- `grid`: a width×height array of `Cell`
- `mode`: Normal | Raw | AltScreen

ftui-specific derived state:
- `link_state`: OSC 8 hyperlink open/close tracking
- `cursor_visible`: bool
- `sync_output`: bool (DEC 2026 nesting/active)
- `scroll_region`: optional (top..bottom) margins

### 1.2 Safety Invariants
- Cursor bounds: `0 <= x < width`, `0 <= y < height`.
- Grid validity: every cell is a valid `Cell` value.
- Mode cleanup: on exit, Raw/AltScreen/mouse/paste/focus modes are restored to safe defaults.

### 1.3 Where This Is Enforced
Type-level (compile-time-ish):
- `TerminalSession` owns terminal lifecycle so that cleanup cannot be “forgotten”.

Runtime checks:
- bounds checks on cursor moves (or explicit clamping policy)
- internal assertions in debug builds for invariants

Tests:
- PTY tests validate cleanup invariants under normal exit + panic.

Implementation module targets (will be updated as code lands):
- Terminal lifecycle + cleanup: `crates/ftui-core/src/terminal_session.rs`
- Capability model: `crates/ftui-core/src/terminal_capabilities.rs`

---

## 2) Rendering Pipeline State Machine

### 2.1 States
States (from plan):
- Idle
- Measuring
- Rendering
- Diffing
- Presenting
- Error

### 2.2 Transitions
- Idle → Measuring (render request)
- Measuring → Rendering (layout complete)
- Rendering → Diffing (draw complete)
- Diffing → Presenting (diff computed)
- Presenting → Idle (present complete)
- * → Error (I/O error, internal invariant violation)
- Error → Idle (recover)

### 2.3 Pipeline Invariants
I1. In Rendering state, only the back buffer is modified.
I2. In Presenting state, only ANSI output is produced.
I3. After Presenting, front buffer equals desired grid.
I4. Error state restores terminal to a safe state.
I5. Scissor stack intersection monotonically decreases on push.
I6. Opacity stack product stays in [0, 1].

### 2.4 Where This Is Enforced
Type-level:
- Separate “front” vs “back” buffers owned by Frame/Presenter APIs.

Runtime checks:
- scissor stack push/pop asserts intersection monotonicity in debug
- opacity stack push/pop clamps and asserts range

Tests:
- executable invariant tests (bd-10i.13.2)
- property tests for diff correctness (bd-2x0j)
- terminal-model presenter roundtrip tests (bd-10i.11.1)

Implementation module targets (will be updated as code lands):
- Buffer/Cell invariants: `crates/ftui-render/src/buffer.rs`, `crates/ftui-render/src/cell.rs`
- Diff engine: `crates/ftui-render/src/diff.rs`
- Presenter: `crates/ftui-render/src/presenter.rs`

---

## 3) Notes for Contributors

- The goal is not “perfect formalism”; the goal is to prevent drift.
- If you change behavior in Buffer/Presenter/TerminalSession, update this document and add tests.
