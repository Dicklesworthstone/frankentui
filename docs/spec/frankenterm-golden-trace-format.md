# Golden Trace Format (Native + Web) — bd-lff4p.5.1

Goal
- Define a stable, versioned trace format that can reproduce bugs and gate regressions across:
  - native terminal sessions (PTY-backed)
  - web sessions (FrankenTerm.WASM)
  - remote sessions (browser <-> PTY bridge)

Design constraints
- Forward-migratable (schema_versioned, strict validation, additive evolution).
- Reducible/minimizable (delta-debuggable traces).
- Deterministic replay (explicit seed, explicit time).
- Audit-friendly (JSONL; debuggable diffs; payloads stored as separate files).

Non-goals
- Capturing "everything": traces capture *only* the explicit inputs needed to replay deterministically.

---

## 1) File Layout

A trace is a directory bundle:

- `trace.jsonl` (required): one JSON object per line.
- `payloads/` (optional): binary payloads referenced by `trace.jsonl`.

Notes:
- JSONL is chosen so traces can be streamed and incrementally minimized.
- Payloads are separate so large blobs don't bloat diffs.

---

## 2) Schema Versioning

Every JSONL line MUST include:
- `schema_version`: e.g. `golden-trace-v1`
- `event`: record type discriminator (see below)

Evolution rules:
- Additive only within a `vN` schema: new optional fields allowed, never repurpose meaning.
- Breaking changes require `v(N+1)` with a migration note.
- Validators MUST reject unknown record types for a given schema_version (fail fast).

---

## 3) Determinism Contract

Replay must be deterministic given:
- `seed` (explicit, required; `0` allowed).
- `clock` model and event timestamps:
  - timestamps are relative to trace start (`ts_ns`), not wall clock.
  - "tick" events advance time explicitly.
- `profile/capabilities`:
  - traces record the capability profile used; replay uses the same profile.

No implicit time:
- The system MUST NOT call global time sources during replay (no `Instant::now()` without indirection).

---

## 4) Record Types (golden-trace-v1)

### 4.1 Header

Exactly one per file; must be first.

```json
{"schema_version":"golden-trace-v1","event":"trace_header", ...}
```

Required fields:
- `run_id` (string): stable identifier.
- `git_sha` (string): commit under test.
- `seed` (number): deterministic seed.
- `env` (object): `target` ("native"|"web"|"remote"), plus OS/arch when available.
- `profile` (string): capability profile name (e.g., "modern", "tmux", "dumb").

Recommended fields:
- `term` / `colorterm` / mux flags (native/remote)
- `dpr` + `font_metrics` + `zoom` (web)
- `policies` (object): diff/coalescing/degradation knobs

### 4.2 Input Events

Input records capture the *effective* input semantics needed for replay.
For web/remote, prefer recording post-normalization (after browser key mapping).

```json
{"schema_version":"golden-trace-v1","event":"input","ts_ns":16000000,"kind":"key", ...}
```

Required fields:
- `ts_ns` (number): timestamp in nanoseconds since trace start.
- `kind` (string): `key` | `mouse` | `wheel` | `paste` | `composition` | ...

Recommended fields:
- For byte-level encodings:
  - `bytes_b64` (string) and `encoding` (string), so replay can feed bytes directly.
- For semantic encodings:
  - normalized key code + modifiers, or normalized mouse cell coordinates.

### 4.3 Resize Events

```json
{"schema_version":"golden-trace-v1","event":"resize","ts_ns":0,"cols":120,"rows":40}
```

Required fields:
- `ts_ns`, `cols`, `rows`

### 4.4 Tick / Time Step Events

```json
{"schema_version":"golden-trace-v1","event":"tick","ts_ns":32000000}
```

Required fields:
- `ts_ns`

Guidance:
- Use ticks to force deterministic scheduling in the runtime (both native and wasm).

### 4.5 Frame Checkpoints (Golden Gates)

Frame records are the "gates": replay must reproduce these hashes.

```json
{"schema_version":"golden-trace-v1","event":"frame","frame_idx":1,"ts_ns":16000000, ...}
```

Required fields:
- `frame_idx` (number): monotonic 0-based.
- `ts_ns` (number): time at present.
- `hash_algo` (string): `sha256` (default).
- `frame_hash` (string): hex.

Recommended fields:
- `patch_hash` (string): hex hash of the patch/diff representation (optional).
- `checksum_chain` (string): hex hash chaining all prior frames (optional but recommended).
- `cells_changed` / `diff_runs` / `present_bytes` (debug/perf).
- `payload_kind` + `payload_path` when a binary payload is emitted.

### 4.6 Summary

Exactly one per file; must be last.

```json
{"schema_version":"golden-trace-v1","event":"trace_summary","total_frames":123, ...}
```

Required fields:
- `total_frames`
- `final_checksum_chain` (if chaining is enabled)

---

## 5) Hashing + Normalization

Canonical gate:
- `frame_hash` MUST be computed over a normalized representation of the terminal/grid state.

Normalization rules MUST be explicit and stable:
- stable representation for wide/continuation cells
- stable hyperlink IDs / ordering (no hash-map iteration leakage)
- canonical line endings

Recommendation:
- Prefer `sha256` for gate hashes (collision resistance).
- Permit an additional fast hash (u64) for local debugging, but do not gate CI on it.

Checksum chaining:
- `checksum_chain = sha256(prev_chain || frame_hash)`
- `prev_chain` is initialized from the header in a deterministic way (documented in code).

---

## 6) Payloads (Optional)

Payloads exist to make mismatches debuggable without re-running:
- `diff_runs_v1` (binary) for patch-based replay
- `full_buffer_v1` (binary) for full-state snapshots

Rules:
- payload paths are relative to the trace bundle directory.
- payload formats are versioned by `payload_kind`.

Existing reference implementation (ftui today):
- `crates/ftui-runtime/src/render_trace.rs` (`schema_version="render-trace-v2"`)

`golden-trace-v1` may supersede `render-trace-v2` for FrankenTerm, but the payload
mechanics (JSONL + sidecar payload directory) should remain compatible.

---

## 7) Validation + CI Gates

Validation requirements:
- Strict schema validation in CI.
- On mismatch: print first failing frame index and provide artifact paths.

Native alignment (existing patterns):
- E2E JSONL schema: `tests/e2e/lib/e2e_jsonl_schema.json`
- Validator: `tests/e2e/lib/validate_jsonl.py`

Trace gates should:
- emit `artifact` events in E2E JSONL referencing `trace.jsonl` + payload directory
- validate trace schema + validate frame hashes against an append-only registry

---

## 8) Minimization / Delta Debugging

Traces must be amenable to automatic minimization:
- "Remove an input/tick segment" and check if mismatch persists.
- "Bisect frames" to find earliest divergence.

Practical rule:
- Prefer recording input at the highest semantic level that still reproduces deterministically.
  - If byte-level is needed (PTY/remote), record bytes.
  - If semantic is stable (web DOM), record semantics post-normalization.

