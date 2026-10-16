# Definition of Done (Ship Checklist)

> "It is not done when it compiles. It is done when it is verified, deterministic, and defensible."

This document defines the strict criteria for marking a task, feature, or bug fix as "Done" in the FrankenTUI project. These standards enforce our "Alien Artifact" quality bar.

---

## 1. Correctness & Safety

- [ ] **No Unsafe Code:** Every workspace crate enforces `#![forbid(unsafe_code)]`; `unsafe` blocks are forbidden everywhere in production code (including FFI).
- [ ] **One-Writer Rule:** All terminal output is routed through `TerminalWriter`. No direct `println!`, `eprintln!`, or raw byte writes to stdout/stderr.
- [ ] **RAII Cleanup:** If the feature interacts with terminal state (raw mode, alt-screen, mouse capture), it MUST use `TerminalSession` or equivalent RAII guards to guarantee restoration on exit/panic.
- [ ] **Panic Safety:** The code must not panic on user input or external state (e.g., window size 0). Use `Result` for recoverable errors.
- [ ] **16-Byte Cell:** Any changes to `Cell` or `Buffer` MUST preserve the 16-byte `Cell` layout (verified via `std::mem::size_of`).

## 2. Determinism & Stability

- [ ] **Flicker-Free:** Rendering MUST be double-buffered and diff-based. No partial frame writes.
- [ ] **Deterministic Output:** Given the same input events and seed, the system MUST produce identical output.
- [ ] **Stable Sorting:** All UI lists, search results, and Z-ordered elements MUST use stable sorting to prevent jitter.
- [ ] **Conformal Bounds:** Probabilistic decisions (e.g., resize coalescing, ranking) MUST use conformal prediction or rigorous bounds, not ad-hoc heuristics.

## 3. Performance & Efficiency

- [ ] **Zero-Allocation Hot Paths:** The render loop (Model `view` -> Frame `render`) MUST NOT perform per-cell heap allocations.
- [ ] **Budget Compliance:** The feature must fit within the frame budget (typically 16ms). Heavy computations must be amortized or offloaded.
- [ ] **Sparse Updates:** If the feature supports it, it should correctly set `dirty_rows` or use `BufferDiff` to minimize I/O.

## 4. "Alien Artifact" Engineering

- [ ] **Evidence Ledgers:** Any complex probabilistic decision (BOCPD, fuzzy scoring, coalescing) MUST emit a structured "evidence ledger" (JSONL) explaining *why* a decision was made.
- [ ] **Math over Magic:** Use principled algorithms (Bayesian inference, PID controllers, CUSUM) instead of magic constants/heuristics where possible.
- [ ] **Telemetry:** Key events (state changes, errors, performance thresholds) MUST be instrumented with `tracing` or internal telemetry hooks.

## 5. Testing & Verification

- [ ] **Unit Tests:** Core logic is covered by `#[test]` functions.
- [ ] **Snapshot Tests:** UI components have `insta` snapshot tests validating their visual output.
- [ ] **Doc Tests:** Public API examples in documentation are executable and pass.
- [ ] **Build Cleanliness:** `cargo check`, `cargo clippy`, and `cargo fmt` pass without warnings.
- [ ] **Regression:** If fixing a bug, a regression test case (unit or E2E) MUST be added to prevent recurrence.

## 6. Documentation

- [ ] **Intent & Invariants:** Code comments explain *why* and *how* (invariants), not just *what*.
- [ ] **Module Docs:** New modules have top-level `//!` documentation explaining their role and usage.
- [ ] **Specs:** Significant architectural changes are reflected in `docs/spec/`.

---

## 7. Stop-Ship Criteria (Quality Gates)

A release (or merge to main) cannot proceed if:

1.  **Build Fails:** Any target (including WASM/Windows if supported) fails to compile.
2.  **Tests Fail:** Any unit, integration, or snapshot test fails.
3.  **Lints Fail:** `clippy` reports warnings (we run with `-D warnings`).
4.  **Verification Gap:** `doctor_frankentui` (if applicable) reports verification failures.
5.  **Telemetry Silence:** New complex features lack observability hooks.

---

## Checklist for Agents

Before reporting a task as complete:

1.  Did you run `cargo check` and `cargo clippy`?
2.  Did you run `cargo test` (or verify via static analysis if env is restricted)?
3.  Did you add a test case for your change?
4.  Did you verify no new allocations were introduced in the hot path?
5.  Did you update relevant documentation?
