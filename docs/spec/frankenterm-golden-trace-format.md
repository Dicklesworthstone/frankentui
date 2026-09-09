# Golden Trace Format (Native + Web) — bd-lff4p.5.1

`ftui-web::session_record` implements the `golden-trace-v2` web session format
specified below. Native/remote bundle integration, sidecar payloads, and
browser/renderer verification remain separate obligations. Registry and schema
tests establish format acceptance; they do not prove replay or browser execution.

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
- The in-tree web recorder currently produces JSONL with inline semantic input
  data. It does not create the broader bundle or sidecar payloads described here.

---

## 2) Schema Versioning

Every JSONL line MUST include:
- `schema_version`: `golden-trace-v2` for the implemented web trace
- `event`: record type discriminator (see below)

Evolution rules:
- Additive only within a `vN` schema: new optional fields allowed, never repurpose meaning.
- Breaking changes require `v(N+1)` with a migration note.
- Validators MUST reject unknown record types for a given schema_version (fail fast).

The v2 reader accepts exactly `golden-trace-v2`. Older v1 and newer v3 data are
incompatible and require an explicit migration. V1 recorded frame checkpoints
without every execution boundary, so final non-rendering steps and their clock
values cannot be reconstructed reliably. Re-record from the original workload
and observed execution; do not relabel v1 lines or infer missing steps from frames.

---

## 3) Determinism Contract

Replay must be deterministic given:
- `seed` (explicit, required; `0` allowed).
- `clock` model and event timestamps:
  - timestamps are relative to trace start (`ts_ns`), not wall clock.
  - each `step` records the actual clock as `clock_secs` and `clock_subsec_nanos`.
  - replay restores that clock at the recorded boundary; `tick.ts_ns` is metadata
    and need not equal the clock accumulated from host time deltas.
- `profile/capabilities`:
  - traces record the capability profile used; replay uses the same profile.

The caller supplies a model with the same seed and settings; the generic web
replay API does not configure a model from header metadata.

No implicit time:
- The system MUST NOT call global time sources during replay (no `Instant::now()` without indirection).

---

## 4) Implemented Web Record Types (golden-trace-v2)

### 4.1 Header

Exactly one per file; must be first.

```json
{"schema_version":"golden-trace-v2","event":"trace_header","seed":42,"cols":80,"rows":24,"env":{"target":"web"},"profile":"modern"}
```

Fields emitted by the web recorder:
- `seed` (number): deterministic seed.
- `cols`, `rows` (numbers): initial terminal dimensions.
- `env` (object): `target: "web"`.
- `profile` (string): capability profile label.

Additional metadata goals for native/remote bundles:
- `run_id` and `git_sha`: stable run identifier and commit under test.
- `term` / `colorterm` / mux flags (native/remote)
- `dpr` + `font_metrics` + `zoom` (web)
- `policies` (object): diff/coalescing/degradation knobs

### 4.2 Input Events

Input records capture the *effective* input semantics needed for replay.
For web/remote, prefer recording post-normalization (after browser key mapping).

```json
{"schema_version":"golden-trace-v2","event":"input","ts_ns":16000000,"data":{"kind":"key","code":"char:a","modifiers":0,"event_kind":"press"}}
```

Required fields:
- `ts_ns` (number): timestamp in nanoseconds since trace start.
- `data` (object): canonical Rust event representation, with `kind` equal to
  `key`, `mouse`, `resize`, `paste`, `ime`, `focus`, `clipboard`, or `tick`.

This schema differs from the browser's encoded-input JSON: for example, browser
`composition` maps to canonical `ime` before recording. Input and resize records
are appended only after admission succeeds. Rejected input is not replayed.

### 4.3 Resize Events

```json
{"schema_version":"golden-trace-v2","event":"resize","ts_ns":0,"cols":120,"rows":40}
```

Required fields:
- `ts_ns`, `cols`, `rows`

### 4.4 Tick / Time Step Events

```json
{"schema_version":"golden-trace-v2","event":"tick","ts_ns":32000000}
```

Required fields:
- `ts_ns`

This records a host time-advance operation's timestamp. The subsequent observed
step's clock fields carry the actual execution time; replay must not substitute
`ts_ns` for that clock or execute an extra step at a tick record.

### 4.5 Observed Execution Boundaries

Every actual initialization or step has one `step` record, including idle,
non-rendering, quitting, and subsequent stopped calls. Example initialization:

```json
{"schema_version":"golden-trace-v2","event":"step","step_idx":0,"ts_ns":0,"init":true,"clock_secs":0,"clock_subsec_nanos":0,"running":true,"rendered":true,"events_processed":0,"events_pending":0,"frame_idx":1}
```

Required fields:
- `step_idx` (`u64`): contiguous, starting at zero.
- `ts_ns` (`u64`): recording timestamp.
- `init` (boolean): true only for step zero, which records actual initialization.
- `clock_secs` (`u64`), `clock_subsec_nanos` (`u32`, less than 1,000,000,000):
  the actual deterministic `Duration` at this boundary.
- `running`, `rendered` (booleans): observed `StepResult` flags.
- `events_processed` (`u32`): admitted terminal events consumed by this call.
- `events_pending` (`u32`): accepted events still queued after the call.
- `frame_idx` (`u64`): rendered-frame count after the call.

Replay invokes initialization or `step()` exactly at these records and compares
the complete result. Frame records never trigger execution. No additional step
may be inserted to drain an overfull batch or process a final input tail.

### 4.6 Frame Checkpoints (Golden Gates)

A frame record must immediately follow each boundary with `rendered: true`;
non-rendering boundaries have none. Initialization that quits produces no frame.

Required fields:
- `frame_idx` (`u64`): contiguous zero-based frame index; one less than the
  preceding rendered boundary's frame count.
- `ts_ns` (`u64`): recording timestamp at present.
- `hash_algo`: `"fnv1a64"`.
- `frame_hash`, `checksum_chain`: 16-digit hexadecimal strings.

Frame checksums verify the existing buffer produced by the preceding boundary.
They do not create render work or establish that a browser displayed the frame.

### 4.7 Summary and Unprocessed Input

Exactly one per file; must be last.

```json
{"schema_version":"golden-trace-v2","event":"trace_summary","total_frames":0,"final_checksum_chain":"0000000000000000"}
```

Required fields:
- `total_frames`
- `final_checksum_chain` (16-digit hexadecimal string)

The summary retains frame and chain totals. Input, resize, or time records without
a subsequent observed boundary are structurally incomplete and must be rejected.
A recorded quit preserves the accepted FIFO tail without updating the model or
executing its effects. The last boundary reports `events_pending`; replay returns
the exact `unprocessed_events` and final running state. A matching replay is not
a claim that those returned events ran: the caller must explicitly retain them
for recovery or report their non-delivery. Never silently discard them.

---

## 5) Hashing + Normalization

Implemented web v2 gate:
- `frame_hash` uses `ftui_runtime::render_trace::checksum_buffer`: FNV-1a 64-bit
  over row-major semantic cell content, colors, and packed attributes, resolving
  grapheme IDs through the supplied pool.
- `checksum_chain = fnv1a64(le_u64(previous_chain) || le_u64(frame_hash))`,
  with each pair hashed from the FNV offset basis and the first previous chain zero.
- The checksum algorithm is unchanged from the v1 web writer.

Normalization rules MUST be explicit and stable:
- stable representation for wide/continuation cells
- stable hyperlink IDs / ordering (no hash-map iteration leakage)
- canonical line endings

SHA-256 frame hashes and chaining remain a broader native/remote bundle design
goal. They are not the implemented web v2 gate and must not be substituted for
FNV checksums or patch-batch hashes without a separate format change.

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

The web `golden-trace-v2` recorder does not implement these sidecar payloads or
replace `render-trace-v2`. Bundle integration remains separate work.

---

## 7) Validation + CI Gates

Validation requirements:
- Strict schema validation in CI.
- On mismatch: print first failing frame index and provide artifact paths.
- For web v2: validate contiguous init/step/frame boundaries, exact step results,
  recorded clocks, pending-input disposition, and frame/summary checksums.
- Registry acceptance alone is not proof of replay, browser/GPU execution, or Shadow.

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
