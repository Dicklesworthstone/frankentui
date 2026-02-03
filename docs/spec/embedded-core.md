# Embedded Core Boundaries (bd-1ai1.1)

## Goal
Define which functionality must be host-agnostic and IO-abstracted for an embedded core crate, and which stays in host adapters (terminal, PTY, GUI, WASM bindings).

## Boundary Criteria
A component belongs in the embedded core if:
- It is deterministic and testable without real IO.
- It depends only on pure data + explicit inputs (time, events, buffers).
- It can operate with abstract traits for clock, input, output, storage, and random.
- It provides value across multiple hosts (terminal, GUI, WASM, embedded devices).

A component is host-specific if:
- It directly controls terminal state, raw mode, or platform capabilities.
- It interprets OS-specific events or provides OS-specific IO.
- It emits ANSI/escape sequences or depends on crossterm.

## Proposed Boundary Map

### Embedded Core (host-agnostic)
- **Geometry + layout**
  - `ftui-layout`: constraint solvers, flex/grid, intrinsic sizing
  - `ftui-core` geometry types (`Rect`, `Size`, `Point`)
- **Text + styling**
  - `ftui-text`: spans, wrapping, bidi, grapheme width
  - `ftui-style`: style flags, color tokens, theme data (no ANSI emission)
- **Render kernel (pure)**
  - `ftui-render` core: `Cell`, `Buffer`, `Diff` computation
  - Frame composition and clipping stacks
- **Widget rendering**
  - `ftui-widgets`: rendering into `Frame` / `Buffer`
  - No direct terminal access; uses core primitives only
- **Runtime model contract**
  - Model traits (`init/update/view/subscriptions`) and message types
  - Command/subscription interfaces as pure enums/traits (no OS handles)

### Host Adapters (platform-specific)
- **Terminal IO + capabilities**
  - `ftui-core::terminal_session` (crossterm raw mode, alternate screen)
  - Input event parsing and key code normalization
  - Terminal capability detection
- **Presenter/output**
  - `ftui-render::presenter` (ANSI emission, cursor control)
  - `ftui-runtime::terminal_writer` (one-writer rule enforcement)
- **Schedulers + timers with OS hooks**
  - Tick timers, sleep, async IO integration (std::time / tokio)
- **PTY + harness**
  - `ftui-pty`, demo harness, snapshot runners

### Hybrid (core API + host impls)
These should be trait-first in core, with host-specific impls:
- **Clock**: `Clock::now()` for deterministic replay tests
- **Input source**: `InputSource::poll()` to decouple crossterm
- **Output sink**: `OutputSink::write(bytes)` to decouple ANSI emission
- **Filesystem**: only for optional assets/logging

## Invariants
- Core modules never import `crossterm` or platform-specific IO.
- Core operations must be deterministic given the same inputs, time, and seed.
- Rendering is pure: `Buffer` + `Diff` do not emit side effects.

## Failure Modes
- **Host adapter failure**: terminal init fails → return error, core remains usable.
- **Capability mismatch**: host detects limited terminal → degrade features via core flags.
- **Clock/input jitter**: host adapter must normalize to stable time/event streams.

## Evidence Ledger (Decision Rationale)
- **Portability**: layout/text/render/widgets are reusable across hosts; terminal IO is not.
- **Testability**: embedded core allows headless tests and deterministic snapshots.
- **Performance**: buffer/diff are CPU-bound and benefit from reuse across platforms.

## Tests & Validation
- Unit tests for core modules must not require a terminal.
- Integration tests for host adapters remain in harness/PTY crates.
- Deterministic replay tests: fixed seed + synthetic events → stable buffer checksums.

## E2E Requirements
- Host E2E tests log JSONL with: run_id, env, capabilities, seed, timings, checksums.
- Core-only tests must run with a fake clock and fake input source.

## Next Steps
- bd-1ai1.2: Design host-specific FFI crate layout.
- bd-1ai1.3: Decide placeholder crates vs design-only stubs.
- bd-1ai1.4: Write ADR for SDK modularization using this boundary map.
