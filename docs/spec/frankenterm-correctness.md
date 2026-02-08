# FrankenTerm Correctness Strategy (Invariants, Proptests, Fuzz, Goldens) — bd-lff4p.8

Goal
- Specify correctness guarantees strong enough to justify replacing BOTH Crossterm (native) and xterm.js (web).
- Treat the terminal engine as a deterministic state machine driven only by explicit inputs.

Non-goals
- "Best effort" terminal emulation. We will be conservative, explicit, and testable.
- Backwards compatibility shims. When a rule is wrong, fix the rule and update goldens intentionally.

## 0) Correctness Model

The engine MUST be deterministic given the same:
- Output stream input: VT/ANSI bytes (and/or higher-level patches, if used).
- Input events: keyboard/mouse/touch/IME (or an explicitly defined encoding to bytes).
- Resize events.
- Ticks/time: explicitly injected time source (never implicit global time).
- Capability profile / configuration.

If any of the above are missing from logs, we cannot reproduce bugs reliably.

## 1) "Formal-ish" Invariants (Written Down, Then Enforced)

### 1.1 Parser State Machine Safety
- No panics for any byte stream (including adversarial input).
- Bounded memory growth:
  - bounded-length CSI/OSC/DCS buffers
  - bounded nesting / recursion (prefer iterative parsing)
  - bounded scrollback growth (explicit capacity policy)
- Valid transitions only:
  - intermediate states cannot be skipped by malformed bytes
  - cancellation rules are explicit (e.g., CAN/ESC semantics)
- Progress guarantee:
  - parser must make forward progress or intentionally reject input with a bounded "drop" policy.

### 1.2 Grid Mutation Safety
- Cursor bounds: `0 <= x < cols`, `0 <= y < rows` at all times.
- Width invariants:
  - wide glyphs occupy `width` cells (continuation markers must be valid)
  - combining marks never produce invalid placeholder cells
- Mutations are total-order deterministic:
  - no hash-map iteration order leaks into visible output or checksum.

### 1.3 Scrollback + Resize/Reflow Policy
- Capacity is explicit and enforced (never unbounded).
- Resize/reflow has an explicit policy:
  - what reflows vs what is preserved
  - whether selection anchors reflow or stays stable
- Viewport invariants:
  - viewport always references valid lines/spans
  - clamped on shrink; converges on grow

### 1.4 Selection Semantics
- Selection spans scrollback + viewport with stable semantics under:
  - scrolling
  - incremental output
  - resize/reflow (as per explicit policy)
- Copy/paste output is deterministic (same content for same trace).

### 1.5 Hyperlink Semantics (OSC 8)
- OSC 8 open/close tracking is balanced:
  - no dangling link on line wrap or scroll
  - link lifetime rules are explicit
- Link mapping is stable under:
  - scrollback
  - resize/reflow (as per explicit policy)

### 1.6 Output/Presentation Semantics
- If a renderer presents patches/frames:
  - each present is attributable to exactly one engine state (no partial/mixed frames)
  - checksums correspond to a well-defined normalization of state.

### 1.7 Determinism + Observability
- Any randomness is seeded and logged.
- Any time-based decision uses injected time and is logged.
- Any capability-dependent behavior is keyed by an explicit profile and is logged.

Where these invariants should live:
- Type-level where possible (ownership/RAII for cleanup; non-null handles; no multi-writer output).
- Runtime assertions in debug builds for invariants that cannot be typed.
- CI gates + goldens for invariants that must hold in release builds.

Existing adjacent spec for ftui (use as reference patterns):
- `docs/spec/state-machines.md` (state machine framing + invariants + trace format)
- `docs/one-writer-rule.md` (serialization/ownership model)

## 2) Property-Based Testing (Proptest) Strategy

Principles:
- Generators produce valid-enough sequences most of the time, but include a controlled rate of adversarial junk.
- Shrinking MUST produce minimal repros that can be lifted into:
  - a deterministic golden trace
  - a fuzz corpus seed
  - an E2E fixture

### 2.1 Generators (Sketch)
- Byte streams:
  - plain text, SGR, CSI cursor moves, OSC 8, bracketed paste, partial sequences
  - long sequences and "storm" patterns (resize, cursor spam)
- Engine events:
  - resize storms with inter-arrival timing from an injected clock
  - input events mapped to bytes (or semantic events with a deterministic encoder)

### 2.2 Invariant Assertions (Examples)
- Never panic; no OOM-style unbounded growth (enforced via caps/limits).
- Cursor always in-bounds.
- "Apply patch then render" is deterministic:
  - same trace => same checksum chain.
- Idempotence:
  - applying an empty/zero-diff update produces no visible changes.

## 3) Fuzzing Strategy

We already fuzz input parsing for ftui:
- `fuzz/fuzz_targets/fuzz_input_parser*.rs`
- CI fuzz job: `.github/workflows/ci.yml` (quick fuzz run)

FrankenTerm adds new high-value fuzz entrypoints (future crates):
- VT/ANSI parser (byte stream -> ops)
- Grid mutation API (ops -> state) with bounded limits
- Scrollback/reflow policy

Operating rules:
- Every crash becomes a regression fixture:
  - minimal byte file (or JSON trace) is checked into fixtures/corpus
  - a unit/proptest regression test references it
- Corpus hygiene:
  - keep a curated minimal corpus (fast) plus optional larger corpus (local).
- Fuzz smoke in CI:
  - time-bounded per target (e.g., 20s) to prevent runaway runtimes.

## 4) Golden Artifacts (Deterministic Repro + "Never Regress")

### 4.1 Conformance Fixtures (Byte Stream -> Expected Grid)
Artifacts:
- Input: a `.bytes` or `.jsonl` fixture describing bytes + injected events.
- Output: expected normalized grid snapshots (text form) and checksum chain.

Normalization rules must be explicit:
- canonical line endings
- explicit representation for wide/continuation cells
- stable ordering for hyperlink IDs (no hash-order leakage)

### 4.2 Golden Trace Corpus (Event Log -> Expected Checksums)
Artifacts:
- `trace.jsonl` per `docs/spec/frankenterm-golden-trace-format.md` (`schema_version="golden-trace-v1"`):
  - header with `run_id`/`git_sha`/`seed`/`profile`/`env`
  - per-frame hashes (and optional checksum chaining)
  - optional payload references (diff runs / full buffers)

Note:
- `render-trace-v1` (ftui today) is a useful reference implementation of JSONL trace emission + replay.
  FrankenTerm's `golden-trace-v1` is intended to be compatible in spirit (streamable JSONL + sidecar payloads),
  but is a distinct schema.

Trace replay gate:
- deterministic runner replays traces and compares checksum chains.
- on mismatch, emit:
  - first failing frame/event index
  - a minimal diff summary (counts, spans)
  - artifact paths (JSONL, PTY captures when applicable)

### 4.3 Renderer Goldens (WebGPU / Canvas)
When the renderer is pixel-based (web), we still gate deterministically:
- patch hashes (preferred) and/or framebuffer hashes for fixed DPR/size.
- record DPR, font metrics, and GPU adapter metadata in JSONL.

## 5) Differential Testing (High Value)

For parser/engine correctness, differential testing is the fastest way to find edge cases:
- Run the same byte streams through a reference emulator and compare normalized state.
  - candidate references: xterm.js, WezTerm, or another well-tested VT implementation
- Keep the diff domain strict:
  - compare only the subset we claim to support (see bd-lff4p.1.1 support matrix)
  - treat out-of-scope features as "ignored", not "failed"

### 5.1 Baseline Differential Harness (Current)

Current in-tree baselines:
- `crates/frankenterm-core/tests/differential_terminal.rs`
- Reference model: `ftui_pty::virtual_terminal::VirtualTerminal`
- `crates/frankenterm-core/tests/differential_extras_reference.rs`
- Reference model: `ftui_extras::terminal::TerminalState` driven by `ftui_extras::terminal::AnsiParser`
- Compared state: visible grid text + cursor position

Scope of the baseline harness:
- Verifies currently supported parser behavior (printable ASCII + C0 controls currently interpreted in parser tests).
- Tracks currently unsupported escape semantics as explicit known-mismatch fixtures with root-cause notes:
  - `tests/fixtures/vt-conformance/differential/known_mismatches.tsv`

Rule:
- When a known mismatch is fixed in `frankenterm-core`, move that case out of known-mismatch fixtures and into supported differential fixtures.

## 6) E2E + Logging Requirements (Native + Web)

The shared logging contract is JSONL:
- Schema: `tests/e2e/lib/e2e_jsonl_schema.json`
- Summary doc: `docs/testing/e2e-summary-schema.md`
- Validator: `tests/e2e/lib/validate_jsonl.py`

Runs MUST log:
- `run_id`, `git_commit` (and dirty flag), `seed`, `profile`, `cols`/`rows`, `mode`
- deterministic time controls (if used) and their parameters
- per-frame checksums (and chain when applicable)
- artifact paths (trace JSONL, PTY capture, hash registry)

Native requirements:
- `scripts/e2e_test.sh` stays green; emits strict JSONL in CI.

Web requirements (future):
- browser identity + DPR + font metrics/zoom
- per-frame patch hashes and patch statistics
- websocket wire counters + latency histograms for remote mode

## 7) Coverage Matrix (Feature -> Test Artifact)

Legend:
- Unit: Rust unit tests in the owning crate/module.
- Prop: proptest invariant tests.
- Fuzz: cargo-fuzz targets.
- Conformance: byte fixtures -> expected normalized grid snapshots.
- Trace: trace replay gate (checksum chain).
- Native E2E: PTY-backed scripts with JSONL validation.
- Web E2E: browser-run scripts with JSONL validation.

| Feature ("never regress") | Unit | Prop | Fuzz | Conformance | Trace | Native E2E | Web E2E |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Selection + copy/paste | req | req | opt | req | req | req | req |
| OSC 8 hyperlinks (hover/click mapping) | req | req | opt | req | req | req | req |
| Resize storms + reflow policy | req | req | opt | req | req | req | req |
| Scrollback capacity + viewport invariants | req | req | opt | req | req | req | req |
| IME/composition (web) | req | req | opt | req | req | opt | req |
| Focus + mouse modes (capture policy explicit) | req | req | opt | opt | req | req | req |
| Search (scrollback search) | req | req | opt | req | req | req | req |
| Terminal replies (DSR/DA/DEC) | req | req | opt | req | req | req | req |
| Determinism (seed/time/profile) | req | req | req | req | req | req | req |

Notes:
- "opt" means optional initially, but should become "req" for parser/engine entrypoints once stable.
- Differential testing is not listed as a column; treat it as a continuous discovery tool feeding fixtures/goldens.

## 8) CI Hooks (What Exists Now, What Must Exist Before We Ship)

Already in CI (ftui today):
- Format/clippy/tests: `.github/workflows/ci.yml` job `check`
- E2E PTY + JSONL strict validation: `.github/workflows/ci.yml` job `e2e-pty`
- Demo showcase E2E + JSONL strict validation: `.github/workflows/ci.yml` job `demo-showcase`
- Fuzz smoke (input parser): `.github/workflows/ci.yml` job `fuzz`
- WASM build check (host-agnostic crates): `.github/workflows/ci.yml` job `wasm`
  - `cargo check --target wasm32-unknown-unknown -p ftui-core -p ftui-render -p ftui-style -p ftui-layout -p ftui-text -p ftui-i18n`
  - Note: native terminal lifecycle (`ftui_core::terminal_session::TerminalSession`) is intentionally not available on wasm; this gate is about preventing accidental platform coupling in the core surfaces.

Required for FrankenTerm / ftui-web before claiming "replacement-ready":
- WASM build checks (at minimum):
  - `cargo check -p ftui-web --target wasm32-unknown-unknown`
  - `cargo check -p frankenterm-web --target wasm32-unknown-unknown`
- Proptest smoke configuration:
  - CI runs a bounded number of cases (fast) and relies on goldens/E2E for broad coverage.
- Trace replay gate:
  - run on every PR for a curated corpus (fast)
  - nightly/cron runs larger corpus (optional)
- Checksum gate:
  - golden hash registries must be validated in strict mode (already a pattern in existing E2E jobs)

## 9) Next Steps (Dependency-Driven)

This spec unblocks:
- bd-lff4p.5.1 (golden trace format design)
- bd-lff4p.5.8 (extend shared JSONL schema for web/remote)
- bd-lff4p.1.1 (VT/ANSI support matrix + conformance fixtures)
- bd-lff4p.1.3 / bd-lff4p.1.6 (grid + parser implementation)
- bd-lff4p.1.9 / bd-lff4p.1.11 (proptest + fuzz entrypoints)
