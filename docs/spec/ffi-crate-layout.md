# Host-Specific FFI Crate Layout (bd-1ai1.2)

## Goal
Define a crate layout for host-specific bindings (WASM, C, Zig, JVM, etc.) without forcing a universal C ABI that all bindings depend on.

## Principles
- **Host-first bindings**: each host gets a bespoke crate with idiomatic API.
- **No forced C-ABI hub**: avoid a single C FFI layer that all languages must wrap.
- **Core stays lean**: embedded core remains dependency-light and host-agnostic.
- **Feature isolation**: heavy deps (wasm-bindgen, JNI, libffi) only in host crates.

## Proposed Crate Names
- `ftui-core` (host-agnostic core utilities + geometry + events)
- `ftui-render` (pure buffer + diff)
- `ftui-layout` (constraint solvers)
- `ftui-text` (spans, wrapping, bidi)
- `ftui-style` (styles + color tokens)
- `ftui-widgets` (render-to-frame widgets)
- `ftui-runtime` (model/update loop, host-agnostic interfaces)
- `ftui-ffi-wasm` (WASM bindings, JS-facing API)
- `ftui-ffi-c` (C ABI for embedded / C callers)
- `ftui-ffi-zig` (Zig package and bindings)
- `ftui-ffi-jvm` (JNI/Java/Kotlin bindings)
- Optional future: `ftui-ffi-python`, `ftui-ffi-dotnet`

## Dependency Graph (High-Level)
```
ftui-core  ftui-render  ftui-layout  ftui-text  ftui-style
     \        |            |            |          |
               -------- ftui-widgets --------
                          |
                      ftui-runtime
                          |
    ------------------------------------------------------
    |            |              |              |         |
ftui-ffi-wasm  ftui-ffi-c   ftui-ffi-zig  ftui-ffi-jvm  (others)
```

## Minimal Public APIs

### Common Core API Surface
- Buffer/frame: create, render, diff (no ANSI emission)
- Layout: constraints, flex/grid
- Text: spans, wrapping, bidi
- Widget: render into `Frame`
- Runtime: model/update/view/subscriptions (pure commands)

### WASM (`ftui-ffi-wasm`)
- `WasmProgram` wrapper with JS-friendly calls
- APIs accept JSON or JS arrays; return image buffer or diff patches
- No direct terminal control; consumer decides rendering

### C (`ftui-ffi-c`)
- Stable C ABI with opaque handles
- Functions: `ftui_program_new`, `ftui_program_update`, `ftui_program_render`
- Expose raw buffer + diff output for host rendering
- Avoid crossterm/ANSI emission in this crate

### Zig (`ftui-ffi-zig`)
- Zig package with bindings to `ftui-ffi-c`
- Zig-native wrappers for `Buffer` and `Diff`
- Optional direct Rust->Zig if future non-C ABI is desired

### JVM (`ftui-ffi-jvm`)
- JNI bindings with Java/Kotlin-friendly structs
- Expose `ByteBuffer` for buffer/diff output
- Lifecycle functions align with JVM memory management

## Invariants
- FFI crates must not pull in terminal IO (crossterm).
- FFI crates must not leak Rust lifetimes into host APIs.
- All FFI entrypoints must be deterministic given inputs + seed.

## Failure Modes
- **FFI init fails**: return explicit error codes; no panic.
- **Host alloc mismatch**: only pass byte buffers with explicit sizes.
- **ABI drift**: versioned symbols + build-time checks.

## Tests & Validation
- ABI smoke tests per host crate.
- Buffer checksum parity tests across FFI boundaries.
- Deterministic replay fixtures with fixed seeds.

## E2E Requirements
- For each FFI crate, provide a host-level demo harness that logs JSONL:
  - run_id, env, seed, timings, checksums, capabilities, outcome

## Next Steps
- bd-1ai1.3: Decide placeholder crates vs design-only stubs.
- bd-1ai1.4: Write ADR for SDK modularization.
- bd-1ai1.5: Define test strategy for embedded core + host bindings.
