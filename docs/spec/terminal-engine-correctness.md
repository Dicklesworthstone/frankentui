# Terminal Engine Correctness Strategy (Invariants, Fuzz, Goldens) — bd-lff4p.8

## Goal

Specify correctness guarantees strong enough to justify replacing both:

- Native terminal backend (today: `crossterm`)
- Web terminal engine (today: `xterm.js`)

This spec treats a "terminal engine" as a deterministic state machine. The core
promise is: given the same explicit inputs, we produce the same observable
outputs, and we never corrupt internal state.

## Correctness Model

### Deterministic State Machine

The engine is driven only by explicit inputs:

- Output byte streams (VT/ANSI) and/or higher-level "patches"
- Input events (keyboard/mouse/touch/IME)
- Resize events
- Ticks/time (explicitly injected; never implicit global time)

No hidden I/O. No reliance on wall-clock time. No ambient global state.

### Observable Outputs

At minimum, the following must be observable and testable:

- Grid state (cells, attributes, cursor position, modes)
- Scrollback state (line storage, viewport, wrap/reflow semantics)
- Selection state (spans, anchors, stable behavior under resize where specified)
- Hyperlink state (OSC 8 open/close semantics and lifetime mapping)
- Presentation artifacts (frame buffers and/or minimal patches/diffs)
- Evidence logs (JSONL) proving determinism and gating decisions

## Invariants (Written Down, Then Enforced)

These invariants are the "contract" that all tests (unit/property/fuzz/e2e) must
prove continuously.

### Parser State Machine Safety

- No panics for any input (including adversarial).
- Bounded memory growth:
  - input buffering must be capped (DoS limits)
  - intermediate structures must have explicit maxima
- Valid transitions only:
  - CSI/OSC/DCS parsing is a finite state machine with total transition coverage
  - invalid sequences are handled by deterministic error recovery

Existing related components:

- `crates/ftui-core/src/input_parser.rs`
- Fuzz targets under `fuzz/` (see `cargo fuzz list`)

### Grid Mutation Correctness

- Cursor always in-bounds after any operation.
- Width/height invariants are maintained:
  - grid dimensions are consistent with allocated storage
  - no "out of range" reads/writes
- Wide glyph semantics:
  - continuation cells are consistent and never become "dangling"

Existing related components:

- `crates/ftui-render/src/buffer.rs`
- `crates/ftui-render/src/cell.rs`

### Scrollback Correctness

- Capacity is enforced deterministically (no unbounded growth).
- Wrap/reflow policy is explicit and stable.
- Viewport invariants:
  - top/bottom bounds are consistent
  - resizing preserves the documented semantics (including edge cases)

Existing related E2E coverage:

- `tests/e2e/scripts/test_inline.sh`
- `tests/e2e/scripts/test_resize_scroll_region.sh`
- `tests/e2e/scripts/test_golden_resize.sh`

### Selection Semantics

- Selection spans scrollback + viewport (as specified).
- Selection remains stable under resize where specified:
  - anchors reflow deterministically
  - selection does not "jump" due to unrelated redraws
- Copy/paste is faithful to the selected content.

Existing related E2E coverage:

- `tests/e2e/scripts/test_paste.sh`
- `tests/e2e/scripts/test_text_editor.sh`

### Hyperlink Semantics (OSC 8)

- OSC 8 open/close are tracked deterministically:
  - links open and close in well-nested fashion
  - link lifetime is explicit and visible in evidence logs
- Link mapping (link_id <-> URL) is stable and does not leak across frames.

Existing related components/tests:

- `crates/ftui-render/src/link_registry.rs`
- `crates/ftui-render/src/presenter.rs`
- `tests/e2e/scripts/test_osc8_hyperlinks.sh`

### Input Modes and Focus/Mouse Modes

- Mouse modes (SGR, capture policies) behave deterministically.
- Focus events are handled and logged deterministically.
- Keyboard modes (kitty keyboard protocol, paste mode) are handled with explicit
  capability gating.

Existing related E2E coverage:

- `tests/e2e/scripts/test_mouse_sgr.sh`
- `tests/e2e/scripts/test_focus_events.sh`
- `tests/e2e/scripts/test_kitty_keyboard.sh`

### IME / Composition

IME/composition events must be:

- Represented explicitly in the input/event model.
- Logged with stable semantics (start/update/commit/cancel).
- Replayable from traces without a live OS IME.

Note: this repo currently emphasizes terminal-driven input. IME semantics should
be specified here first, then implemented as a host adapter capability.

## Test Strategy

The correctness strategy is layered. Each layer catches different classes of
bugs; no single layer is sufficient.

### Unit Tests

Primary use: local invariants and pure functions. Fast and targeted.

Examples already present in this repo:

- Diff soundness/unit tests: `crates/ftui-render/src/diff.rs`
- Capability/event parsing: `crates/ftui-core/src/event.rs`

Reference matrix: `docs/testing/coverage-matrix.md`

### Property Tests (proptest)

Primary use: prove invariants across large randomized spaces and shrink to
minimal repros.

Required for terminal engines:

- Random byte streams with bounded lengths and structured escape sequences.
- Random event streams (keys/mouse/focus/paste) with realistic distributions.
- Resize storms (bursty vs steady) with explicit time steps.

Shrinking requirements:

- Byte stream shrinker must produce a minimal sequence that still violates the invariant.
- Event stream shrinker must preserve causal ordering (e.g., press before release).
- Resize storm shrinker must minimize events while keeping the failure.

### Fuzzing

Primary use: adversarial exploration beyond proptest, especially parser and
state-machine edges.

Strategy:

- Entrypoints:
  - parser-only (bytes -> parser events)
  - engine-step (bytes/events/resizes -> state transitions)
- Corpora:
  - seed with real-world captures (PTY logs, known terminal sequences)
  - seed with minimal "nasty" sequences (truncation, malformed OSC/DCS, huge params)
- Crash triage protocol:
  - every crash becomes a regression fixture (unit test or proptest seed)
  - preserve seed + minimized input in-repo

CI hook exists today:

- `.github/workflows/ci.yml` job `fuzz` runs `cargo fuzz build` and quick fuzz runs.

### Golden Artifacts

Goldens are deterministic, portable artifacts that prevent regressions.

We use three complementary golden families:

1. Conformance fixtures (byte stream -> expected grid snapshots)
   - Stored as small, human-diffable fixtures.
   - Used to validate parsing + grid semantics without live terminal I/O.

2. Golden trace corpus (event log -> expected checksums)
   - Trace includes: input events, resize events, injected ticks, and capability profile.
   - Replay produces stable checksums per step/frame/hash_key.
   - Registry must be explicit and versioned (no silent breakage).

3. Renderer goldens (framebuffer/patch checksums for key scenes)
   - Snapshot tests for canonical screens and sizes.
   - Existing checksum registry: `golden_checksums.txt`

Existing infrastructure to build on:

- JSONL schema + validator:
  - `tests/e2e/lib/e2e_jsonl_schema.json`
  - `tests/e2e/lib/validate_jsonl.py`
  - `tests/e2e/lib/e2e_hash_registry.json`
- E2E playbook: `docs/testing/e2e-playbook.md`

### Differential Testing (High Value)

For a replacement terminal engine, differential tests are the fastest way to
find semantic mismatches.

Strategy:

- Feed identical byte streams and event sequences to:
  - FrankenTerm engine under test (future)
  - One or more reference emulators (native + web)
- Compare a normalized observable representation:
  - grid snapshot (cells + attrs + cursor + modes)
  - scrollback viewport snapshot
- Differences must be categorized:
  - expected divergence (documented)
  - bug (add golden + fix)

## E2E + Logging Requirements

Native requirements:

- `tests/e2e/scripts/*` remains green.
- All runs emit detailed JSONL.
- JSONL conforms to `tests/e2e/lib/e2e_jsonl_schema.json` (extend schema when needed; never break silently).

Web requirements (spec-level for now):

- A trace runner replays the same golden traces and verifies checksums.
- Web runner emits the same JSONL schema and uses the same hash registry.

Every run must log at least:

- `run_id`, `build_id`/git_sha, seed, size, capability profile
- perf counters (if relevant), and checksums per step/frame

## Correctness Coverage Matrix (Feature -> Tests)

This table is the "never forget" mapping from user-facing behavior to coverage.
Where an entry says "future", it must be implemented before declaring a backend
replacement acceptable.

| Feature / Invariant | Unit | Property | Fuzz | E2E | Goldens |
| --- | --- | --- | --- | --- | --- |
| Parser safety (no panic, bounded) | `crates/ftui-core/src/input_parser.rs` | future: structured byte stream generators | `fuzz/*` parser targets | `tests/e2e/scripts/test_input.sh` | future: conformance fixtures |
| Grid bounds + wide glyph semantics | `crates/ftui-render/src/buffer.rs`, `crates/ftui-render/src/cell.rs` | future: random writes + reflow invariants | future: engine-step fuzz | `tests/e2e/scripts/test_unicode.sh` | snapshot checksums |
| Scrollback + inline mode behavior | unit helpers as needed | future: resize storm generator | future | `tests/e2e/scripts/test_inline.sh`, `tests/e2e/scripts/test_resize_storm.sh` | `tests/e2e/scripts/test_golden_resize.sh` |
| Copy/paste | unit helpers as needed | future: selection stability generator | future | `tests/e2e/scripts/test_paste.sh` | trace replay checksums |
| Selection stability under resize | future | future | future | `tests/e2e/scripts/test_text_editor.sh` | future: selection trace corpus |
| OSC 8 hyperlinks | `crates/ftui-render/src/link_registry.rs` | future: nested OSC generation | future | `tests/e2e/scripts/test_osc8_hyperlinks.sh` | snapshot + trace checksums |
| Focus events | `crates/ftui-core/src/event.rs` | future: focus sequences | future | `tests/e2e/scripts/test_focus_events.sh` | trace checksums |
| Mouse modes (SGR, capture policy) | `crates/ftui-core/src/event.rs` | future: mouse stream generators | future | `tests/e2e/scripts/test_mouse_sgr.sh` | trace checksums |
| IME/composition | future | future | future | future | future |
| Cleanup/RAII (terminal restored) | `crates/ftui-core/src/terminal_session.rs` | future: forced panic paths | future | `tests/e2e/scripts/test_cleanup.sh` | golden PTY traces |

## CI Hooks (What Must Be Gated)

To keep the replacement credible, CI must gate the following continuously:

- Format + clippy + unit tests (includes proptest tests)
- Fuzz smoke runs
- Deterministic trace replay and checksum validation
- WASM build checks for host-agnostic crates

This repo already gates most of these in `.github/workflows/ci.yml`. The WASM
target check is added as part of bd-lff4p.8 to prevent accidental host coupling
in the core crates.

## "Never Regress" List (Must Have Goldens)

These behaviors are not allowed to regress without an explicit, reviewed update
to golden artifacts:

- Copy/paste
- Selection semantics (including scrollback span and resize stability)
- OSC 8 hyperlinks
- Resize handling (including storms and scroll region behavior)
- IME/composition semantics
- Focus and mouse modes

When any of these change intentionally:

- Update the relevant golden artifacts.
- Update the hash registry/checksums.
- Record the new semantics in this spec (do not rely on code archaeology).

