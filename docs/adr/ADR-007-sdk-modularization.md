# ADR-007: SDK Modularization (Embedded Core + Host Bindings)

## Context
We want FrankenTUI to be usable beyond a single terminal host while keeping the core deterministic, testable, and dependency-light. A single universal C ABI layer would simplify some bindings but creates tight coupling and forces all languages to inherit C’s constraints. The project also enforces “no file proliferation” and avoids empty crates without a real target.

## Decision
- **Adopt an embedded, host-agnostic core** for layout, text, render buffers, and widget rendering.
- **Implement host-specific bindings as first-class crates** (WASM, C, Zig, JVM, etc.).
- **Avoid a mandatory universal C-ABI hub**; each host binding exposes its own idiomatic API.
- **Defer placeholder crates** until a concrete host target and minimum viable binding/test plan exist.

## Alternatives Considered
1. **Monolithic crate**
   - Pros: simplest packaging
   - Cons: tight coupling to terminal IO; harder to embed or bind
2. **Universal C ABI core**
   - Pros: one ABI for all hosts
   - Cons: forces C constraints across all bindings; brittle API surface
3. **Dynamic plugin model**
   - Pros: extensible, decoupled
   - Cons: complexity, runtime overhead, unclear packaging story

## Consequences
- **More crates**, but lower coupling and clearer boundaries.
- **Host bindings can move independently** without destabilizing core.
- **No implicit promises**: bindings are created only when a host target is real.

## Test Plan / Verification
- Embedded core: unit + property tests with deterministic seeds.
- Host bindings: ABI smoke tests + parity checks against core buffer output.
- E2E: JSONL logs with run_id, env, seed, timings, checksums, capabilities, outcome.

## References
- `docs/spec/embedded-core.md`
- `docs/spec/ffi-crate-layout.md`
- `docs/spec/ffi-placeholder-policy.md`
- `docs/spec/sdk-test-strategy.md`
