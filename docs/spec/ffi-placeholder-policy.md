# Placeholder Crates vs Design-Only (bd-1ai1.3)

## Decision
**Design-only for now.** Do **not** add placeholder crates until there is a concrete host target and a minimum viable binding plan that can ship with tests.

## Rationale
- **AGENTS.md: No File Proliferation** — avoid creating crates that add maintenance overhead without functionality.
- **User confusion risk** — empty crates signal capability that does not exist.
- **Cost control** — each crate adds CI surface, docs, and versioning burden.

## When to Create Placeholder Crates
Create a placeholder crate **only** if all of the following are true:
- A concrete host target is approved (e.g., WASM demo or embedded device).
- Minimal API is defined with an end-to-end demo harness.
- Tests exist (ABI smoke tests + buffer/diff parity).
- CI wiring and packaging expectations are known.

## What “Design-Only” Must Include
To avoid rework later, design-only artifacts must include:
- Crate names + dependency graph (see `docs/spec/ffi-crate-layout.md`).
- Minimal public API sketches for each host.
- Invariants + failure modes (determinism, ABI stability, error handling).
- Test/E2E requirements with JSONL logging schema.

## Invariants
- No placeholder crates without demonstrable host value.
- Any new FFI crate must be small, isolated, and fully tested.

## Failure Modes
- **Premature crate creation** → maintenance debt + broken expectations.
- **Over‑specification** → design becomes brittle without a host driver.

## E2E Guidance
When a host target is selected, add:
- A host harness that exercises one rendering path.
- JSONL logs with run_id, env, seed, timings, checksums, capabilities, outcome.

## Next Steps
- bd-1ai1.4: Write ADR for SDK modularization using this policy.
