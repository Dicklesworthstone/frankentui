# ADR-008: Terminal Backend Strategy (Replace Crossterm, Enable WASM, Unify Interfaces)

Status: ACCEPTED (2026-10-16 — scope: native unix backend via `ftui-tty` + optional `crossterm` + `ftui-web`; unified `BackendPresenter` seam tracked by `bd-g00-root-epic-ewths.17` / G13)
Date: 2026-02-08

## Realization

- **Shipped**: Native Unix backend (`ftui-tty` via `termios`/`poll`/`SIGWINCH`, default on Unix per `bd-g00-root-epic-ewths.1`); optional `crossterm` backend retained for non-Unix/fallback; deterministic step-driven WASM backend (`ftui-web` + `ftui-showcase-wasm`); `ftui-backend` traits (`BackendEventSource`, `BackendPresenter`, `BackendFeatures`).
- **Remaining in G13 (`bd-g00-root-epic-ewths.17`)**: Routing `Program` presentation through `BackendPresenter` across terminal and web hosts.

## Context

FrankenTUI currently depends on:

- **Native terminal I/O** via Crossterm, wrapped by `ftui-core::terminal_session::TerminalSession`.
- **Terminal output** via ANSI emission to `stdout`, coordinated by `ftui-runtime::TerminalWriter`.

The `bd-lff4p` epic ("FrankenTerm.WASM") requires a backend strategy that:

- Replaces both **Crossterm (native)** and **xterm.js (web)** with first-party components.
- Enables the same application code (`Model::update/view`) to run natively and in WASM.
- Preserves FrankenTUI invariants: one-writer rule, deterministic rendering, explicit time, and safe Rust in-tree
  (`#![forbid(unsafe_code)]` in our crates).

This ADR complements ADR-003 (Crossterm as the v1 backend) by defining the v2+ path.

## Decision

### 1. Introduce A Backend Boundary At The Runtime

We will refactor the runtime so `Program` depends on a small backend interface rather than directly constructing and
owning `TerminalSession` + `TerminalWriter`.

The runtime must be able to:

- **Read input** as canonical `ftui_core::event::Event`.
- **Present UI** as `ftui_render::buffer::Buffer` (and optionally a `BufferDiff`).
- **Toggle terminal features** (mouse/paste/focus/kitty keyboard) via a backend-agnostic config struct.
- **Use explicit time** (monotonic) and a platform-specific scheduler/executor (native threads vs WASM event loop).

### 2. Make `ftui-core` Backend-Agnostic (No Crossterm)

`ftui-core` must remain the home for:

- event types (`Event`, `KeyEvent`, `MouseEvent`, etc.)
- parsing/semantic normalization (input parser, coalescers, semantic events)
- capability detection policy and overrides (including environment-based policy)

But `ftui-core` must not own the platform terminal lifecycle and raw event reads long-term.

### 3. Add Platform Crates (Native + Web)

We will introduce:

- `ftui-backend` (new crate): backend traits + small shared structs/enums used by the runtime boundary.
- `ftui-tty` (new crate): native backend implementation (Unix/macOS first, Windows later).
- `ftui-web` (new crate): WASM backend implementation (DOM input + renderer bridge).

`ftui-runtime` will depend on `ftui-backend` and accept any backend implementing the trait(s).

#### Crate Map (Target Dependency Shape)

- `ftui-core`: canonical `Event` types, parsing/semantic normalization, backend-agnostic capability policy.
- `ftui-render` -> `ftui-core`: `Cell`/`Buffer`/`Diff`/`Presenter` (terminal-model-independent kernel).
- `ftui-backend` -> `ftui-core`, `ftui-render`: backend traits + small shared structs at the runtime boundary.
- `ftui-runtime` -> `ftui-backend` (+ `ftui-layout/text/style/widgets` as today): `Program` is backend-driven.
- `ftui-tty` -> `ftui-backend` (+ temporary Crossterm OR first-party native backend): native lifecycle + input + ANSI output.
- `ftui-web` -> `ftui-backend`: WASM driver (DOM input, renderer bridge, explicit clock/executor).
- `ftui-demo-showcase` -> `ftui-runtime` plus a concrete backend (`ftui-tty` for native; `ftui-web` for WASM).

### 4. Isolate Crossterm During Migration

During staged migration, Crossterm (if used at all) must be contained within `ftui-tty` only.

This allows:

- immediate refactors toward a backend boundary without rewriting I/O at the same time
- later replacement of Crossterm with a custom native backend without touching higher layers

This is explicitly a temporary containment step, not a compatibility layer intended to live indefinitely.

### 5. WASM Time + Async Effects: No Threads, Explicit Executor

WASM cannot assume:

- `std::thread`
- blocking sleeps
- synchronous stdin/stdout

We will reshape runtime effects so that side effects are executed by an injected executor. Concretely:

- The runtime core remains a deterministic state machine over `(state, inputs, clock) -> (state, outputs, effects)`.
- Native driver executes effects using threads/worker queues as needed.
- WASM driver executes effects using `spawn_local` and browser timers (no blocking).

For WASM specifically:

- **Ticks are explicit**: time-based behavior is driven by injected `Event::Tick` values.
- **Time is explicit**: all elapsed time queries go through `BackendClock` (no implicit `Instant::now()` in the core loop).
- **Sleep is non-blocking**: "sleep for X" is an effect scheduled by the executor; it never blocks the UI thread.

This may require changing the current `Cmd::Task(TaskSpec, Box<dyn FnOnce() -> M + Send>)` to an effect form that can be
executed on both platforms without blocking the UI.

## Backend Interface Sketch

This is the intended (implementable) shape. Names are provisional; the key is the boundary.

```rust
// ftui-backend
use core::time::Duration;

use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;

#[derive(Debug, Clone, Copy, Default)]
pub struct BackendFeatures {
    pub mouse_capture: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub kitty_keyboard: bool,
}

pub trait BackendClock {
    fn now_mono(&self) -> Duration;
}

pub trait BackendEventSource {
    type Error;

    fn size(&self) -> Result<(u16, u16), Self::Error>;
    fn set_features(&mut self, features: BackendFeatures) -> Result<(), Self::Error>;

    fn poll_event(&mut self, timeout: Duration) -> Result<bool, Self::Error>;
    fn read_event(&mut self) -> Result<Option<Event>, Self::Error>;
}

pub trait BackendPresenter {
    type Error;

    fn capabilities(&self) -> TerminalCapabilities;

    fn write_log(&mut self, text: &str) -> Result<(), Self::Error>;
    fn present_ui(
        &mut self,
        buf: &Buffer,
        diff: Option<&BufferDiff>,
        full_repaint_hint: bool,
    ) -> Result<(), Self::Error>;

    fn gc(&mut self) {}
}

pub trait Backend {
    type Error;
    type Clock: BackendClock;
    type Events: BackendEventSource<Error = Self::Error>;
    type Presenter: BackendPresenter<Error = Self::Error>;

    fn clock(&self) -> &Self::Clock;
    fn events(&mut self) -> &mut Self::Events;
    fn presenter(&mut self) -> &mut Self::Presenter;
}
```

Notes:

- Inline mode is a **native presenter concern** (current `TerminalWriter`) but should remain a configuration option at the
  runtime boundary. Backends that cannot support it must reject explicitly rather than silently degrading.
- Capability detection remains policy-driven; the backend provides a profile and/or raw signals, but higher layers should
  not depend on platform quirks.

## Alternatives Considered

### A. Keep `ftui-core::TerminalSession` As-Is And Add A Separate WASM Stack

Rejected because it:

- duplicates the runtime loop and effect semantics (determinism and golden replay diverge)
- bakes Crossterm assumptions into "core" types long-term
- makes it harder to build a single golden trace corpus usable for both native and web

### B. Rewrite The Native Backend First (Replace Crossterm Immediately)

Rejected as a first step because it couples two large changes:

- refactoring runtime/backend boundaries
- rewriting I/O and lifecycle

This increases risk and slows progress.

### C. Keep Output ANSI-Only And Render WASM Through An ANSI Emulator

Rejected because the project goal is to replace xterm.js and own the renderer. ANSI emulation may exist for
compatibility/testing, but it cannot be the primary path.

## Consequences

### Positive

- Clean platform boundary: backend details stop leaking into core/runtime.
- Enables native + WASM to share the same model/update/view code with explicit time sources.
- Improves testability: backends become mockable; golden traces can replay against the same state machine.
- Crossterm becomes an implementation detail that can be removed without refactoring the whole stack again.

### Negative

- Requires refactoring `ftui-runtime::Program` and potentially `Cmd`/effect execution.
- Adds new crates (`ftui-backend`, `ftui-tty`, `ftui-web`) and some initial integration overhead.

## Migration Plan (With Delete Checkpoints)

The plan is staged to keep the workspace green and avoid long-lived shims.

1. Add `ftui-backend` crate with backend traits and a native adapter implementation that wraps existing code.
2. Refactor `ftui-runtime::Program` to accept a backend instance (native constructors still exist for ergonomics).
3. Introduce `ftui-tty` and move `TerminalSession` and terminal lifecycle into it.
   - At this point, `ftui-core` no longer depends on Crossterm.
4. Add `ftui-web` crate with a driver skeleton (no threads; injected clock + executor).
   - Must compile: `cargo check --target wasm32-unknown-unknown` for relevant crates.
5. Replace Crossterm inside `ftui-tty` with a first-party native backend implementation.
6. Delete the old Crossterm-backed implementation.
   - **File deletion requires explicit written user permission** per `AGENTS.md`.

## Test Plan / Verification

- `cargo check --all-targets` stays green at every stage.
- Add backend mock tests for:
  - feature toggles mapping (`BackendFeatures` -> platform operations)
  - deterministic event delivery under resize storms
  - one-writer rule enforcement at the presenter boundary
- Extend E2E gates:
  - Native: existing `tests/e2e/scripts/*` plus JSONL schema validation.
  - WASM: build checks + trace replay harness that asserts checksums against golden traces.

## Related Docs

- `docs/spec/frankenterm-architecture.md` (see "Backend Trait: Replacing Crossterm" and "Implementation Order").
- `docs/spec/frankenterm-correctness.md` (correctness + golden trace requirements that the backend boundary must support).
