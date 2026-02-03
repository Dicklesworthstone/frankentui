# SDK Test Strategy (Embedded Core + Host Bindings) — bd-1ai1.5

## Goal
Define the test strategy for a future embedded core crate and each host-specific FFI crate (WASM, C, Zig, JVM), without adding implementation yet.

## Principles
- **Deterministic core**: embedded core tests must be pure and reproducible with fixed seeds.
- **Host isolation**: host-specific crates own their integration tests; core remains IO-agnostic.
- **Parity checks**: host bindings validate buffer/diff parity vs core reference output.
- **Logging first**: all E2E runs emit JSONL logs with stable schema.

## Test Matrix

### Embedded Core (host-agnostic)
- **Unit tests**
  - Buffer operations (draw, blend, scissor)
  - Diff computation correctness
  - Layout constraint solving
  - Text wrapping, bidi, grapheme width
- **Property tests**
  - Deterministic output with fixed seed
  - Idempotence of diff on identical buffers
  - Layout invariants (no overlaps, bounds respected)
- **Golden snapshots (headless)**
  - Serialize buffer to text/grid and compare to fixtures
  - No ANSI emission; no terminal IO

### Host Bindings (per crate)
Each host crate must provide:
- **ABI smoke tests**
  - Create/destroy program handles
  - Render once, read buffer/diff
- **Parity tests**
  - Same inputs + seed => buffer checksum matches core reference
- **Error-path tests**
  - Invalid handles return explicit error codes
  - Unsupported capabilities degrade gracefully

### Per-Host Expectations
- **WASM**
  - JS integration tests (node or browser harness)
  - ByteBuffer / typed-array correctness
- **C**
  - C ABI tests (cbindgen + minimal C runner)
  - Symbol versioning check
- **Zig**
  - Zig package tests using C ABI wrapper
- **JVM**
  - JNI smoke tests; ByteBuffer mapping

## JSONL Logging Schema
All E2E runs must log:
- `run_id`, `case`, `env`, `seed`, `timings`, `checksums`, `capabilities`, `outcome`
- Host-specific fields: `ffi_backend`, `abi_version`, `toolchain`

## Failure Modes to Cover
- ABI mismatch / version drift
- Non-deterministic output between core and host
- Host allocation errors (buffer size mismatch, null pointers)
- Capability downgrade handling

## Tooling Recommendations
- **Core**: `proptest` for invariants, `insta` for golden buffers
- **FFI**: minimal host harness per crate + JSONL logger

## Next Steps
- bd-1ai1.4: ADR for SDK modularization
- bd-1ai1.x: implement host harnesses once target host is chosen
