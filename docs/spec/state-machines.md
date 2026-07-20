# State Machines: Terminal + Rendering Pipeline

This document is the formal-ish specification backbone for FrankenTUI.

It is intentionally written to be directly useful for:
- implementation structure (which module owns what)
- invariant placement (type-level vs runtime checks)
- test strategy (unit/property/PTY)

See Bead: bd-10i.13.1.

---

## 1) Terminal State Machine

### 1.1 State Variables
We model the terminal as a state machine that consumes bytes and updates a display grid.

Minimal state (conceptual):
- `cursor`: (x, y)
- `style`: current SGR state (fg/bg/attrs)
- `grid`: a width×height array of `Cell`
- `mode`: Normal | Raw | AltScreen

ftui-specific derived state:
- `link_state`: OSC 8 hyperlink open/close tracking
- `cursor_visible`: bool
- `sync_output`: bool (DEC 2026 nesting/active)
- `scroll_region`: optional (top..bottom) margins

### 1.2 Safety Invariants
- Cursor bounds: `0 <= x < width`, `0 <= y < height`.
- Grid validity: every cell is a valid `Cell` value.
- Mode cleanup: on exit, Raw/AltScreen/mouse/paste/focus modes are restored to safe defaults.

### 1.3 Where This Is Enforced
Type-level (compile-time-ish):
- `TerminalSession` owns terminal lifecycle so that cleanup cannot be “forgotten”.

Runtime checks:
- bounds checks on cursor moves (or explicit clamping policy)
- internal assertions in debug builds for invariants

Tests:
- PTY tests validate cleanup invariants under normal exit + panic.

Implementation module targets (will be updated as code lands):
- Terminal lifecycle + cleanup: `crates/ftui-core/src/terminal_session.rs`
- Capability model: `crates/ftui-core/src/terminal_capabilities.rs`

---

## 2) Rendering Pipeline State Machine

### 2.1 States
States (from plan):
- Idle
- Measuring
- Rendering
- Diffing
- Presenting
- Error

### 2.2 Transitions
- Idle → Measuring (render request)
- Measuring → Rendering (layout complete)
- Rendering → Diffing (draw complete)
- Diffing → Presenting (diff computed)
- Presenting → Idle (present complete)
- * → Error (I/O error, internal invariant violation)
- Error → Idle (recover)

### 2.3 Pipeline Invariants
I1. In Rendering state, only the back buffer is modified.
I2. In Presenting state, only ANSI output is produced.
I3. After Presenting, front buffer equals desired grid.
I4. Error state restores terminal to a safe state.
I5. Scissor stack intersection monotonically decreases on push.
I6. Opacity stack product stays in [0, 1].

### 2.4 Where This Is Enforced
Type-level:
- Separate “front” vs “back” buffers owned by Frame/Presenter APIs.

Runtime checks:
- scissor stack push/pop asserts intersection monotonicity in debug
- opacity stack push/pop clamps and asserts range

Tests:
- executable invariant tests (bd-10i.13.2)
- property tests for diff correctness (bd-2x0j)
- terminal-model presenter roundtrip tests (bd-10i.11.1)

Implementation module targets (will be updated as code lands):
- Buffer/Cell invariants: `crates/ftui-render/src/buffer.rs`, `crates/ftui-render/src/cell.rs`
- Diff engine: `crates/ftui-render/src/diff.rs`
- Presenter: `crates/ftui-render/src/presenter.rs`

---

## 3) Responsive Reflow Spec (Resize / Relayout)

This spec defines the invariants and observable behavior for resize-driven
reflow in FrankenTUI. It is deliberately testable: every rule below maps to
an instrumentation point and at least one unit/property/E2E test scenario.

### 3.1 Goals
- Continuous reflow during resize storms without flicker or ghosting.
- Atomic present: a frame is either fully correct for a given size or not shown.
- No placeholders: never show partial layouts or "blank" regions while reflowing.
- Bounded latency: reflow settles within a defined SLA after the final resize event.

### 3.2 Non-Goals
- Perfect visual smoothness on terminals that lack sync output (DEC 2026).
- Per-terminal pixel-perfect behavior (we operate in cell space).
- GPU-driven animation during resize (CPU baseline only).

### 3.3 Invariants (Must Hold)
R1. **Atomic present**: A present must correspond to exactly one (width, height)
    pair and a fully rendered buffer at that size. Partial or mixed-size output
    is forbidden.
R2. **No placeholder frames**: During a resize storm, either keep the last stable
    frame or present a fully reflowed frame. Never show “empty” filler.
R3. **Final-size convergence**: After the last resize event, the next present
    reflects the final size (no “lagging” intermediate size).
R4. **Inline anchor correctness**: In inline mode, the UI anchor and reserved
    height are recomputed on every size change (no fixed anchor during resize).
R5. **Shrink cleanup**: When the terminal shrinks, output must not leave stale
    glyphs beyond the new bounds (explicit clears or full redraw).
R6. **One-writer rule**: All output (logs + UI) must be serialized via
    `TerminalWriter`, even during resize handling.

### 3.4 Latency SLA
- **Target (p95):** ≤ 120 ms from final resize event → first stable present.
- **Hard cap (p99):** ≤ 250 ms (violations are test failures).
- **Degraded mode:** If the system is over budget, it must drop intermediate
  sizes and jump directly to the final size (still obeying R1–R6).

### 3.5 Atomic Present Rules
1. When a resize event arrives, invalidate the previous buffer and mark a
   reflow-required flag.
2. Do not present until a full layout + render pass completes for the new size.
3. On resize, perform a **full redraw** (diff against empty) to guarantee
   shrink cleanup and eliminate ghosting.
4. Present and flush exactly once for that size; do not interleave logs mid-frame.
5. After present, the front buffer equals the desired grid for that size.

Implementation note (current):
- `Program` currently renders a "Resizing..." placeholder during debounce
  (`crates/ftui-runtime/src/program.rs`). This violates R2 and must be removed
  before the spec is considered fully compliant.

### 3.6 Decision Rule (Resize Coalescing)
Current rule (deterministic, explainable):
- On resize event: update `pending_size` and `last_resize_ts`.
- On tick: if `now - last_resize_ts >= debounce`, apply `pending_size`.
- Always apply the latest size; ignore duplicates.

Evidence ledger / Bayes factor sketch (for future adaptive tuning):
- Hypotheses: `H_ongoing` (user still resizing) vs `H_settled` (resize complete).
- Evidence: inter-arrival time `dt`.
- Bayes factor (simple): `BF_settled = exp(dt / debounce)`, `BF_ongoing = exp(-dt / debounce)`.
- Decision: apply when `BF_settled > BF_ongoing` (equivalently `dt >= debounce`).

### 3.7 Failure Modes (Ledger)
- **Ghosting on shrink**: stale cells remain outside new bounds.
- **Flicker**: partial frame or cursor jumps during reflow.
- **Anchor drift**: inline UI region fails to re-anchor, overwriting logs.
- **Resize lag**: presents intermediate size after final resize.
- **Write interleaving**: log output interspersed with UI present.

For any heuristic (e.g., coalescing/pace control), record an evidence ledger:
- Input signals used (event rate, size delta, time since last present).
- Decision taken (coalesce vs render now).
- Expected impact (latency vs correctness).

### 3.8 Instrumentation Points (Required)
These points MUST emit structured JSONL entries:
1. **Resize event ingress** (raw event read).
2. **Coalescer output** (final size chosen, events collapsed).
3. **Reflow start** (layout + render begin).
4. **Diff stats** (cells changed, runs, bytes).
5. **Present end** (flush complete).
6. **Stability marker** (first stable frame after last resize).

### 3.9 JSONL Log Fields (Required)
Each entry must include:
- `ts_ms`, `event_id`, `phase`
- `cols`, `rows`
- `mode` (`inline` | `alt`)
- `ui_height`, `ui_anchor`
- `coalesced_events`, `coalesce_window_ms`
- `frame_id`, `frame_duration_ms`
- `diff_cells`, `diff_runs`, `present_bytes`
- `sla_budget_ms`, `sla_violation` (bool)
- `ghost_detected` (bool), `flicker_detected` (bool)

### 3.10 Test Plan (Required)
Unit + property tests:
- Event coalescing: idempotence and monotonic convergence to final size.
- Anchor recompute: inline UI start row matches new terminal height.
- Atomic present: no output emitted while in "reflowing" state.
- Shrink cleanup: no cells remain outside new bounds after present.
- Regression fixtures: capture historical resize/flicker bugs with fixed seeds + golden hashes.

E2E PTY scenarios (JSONL logging required):
- **Resize storm**: 5–10 rapid size changes, verify final size + SLA.
- **Shrink → expand**: verify no ghosting after shrink.
- **Inline mode**: logs + UI + resize; verify cursor save/restore and anchor.
- **Alt screen**: no scrollback leakage; present is atomic.
- **Rapid mode switch**: toggle inline/alt during resize; ensure no partial frames.
- **Deterministic mode**: fixed seed + timing controls; logs include seed + checksum.

Instrumentation requirements for tests:
- Capture resize events, frame timestamps, diff sizes.
- Emit flicker/ghosting checksums (per-frame hash of final buffer).

### 3.11 Optimization Protocol (Required)
If performance changes are introduced as part of reflow work:
- **Baseline**: measure p50/p95/p99 reflow latency with `hyperfine` and record raw output.
- **Profile**: collect CPU + allocation profiles; identify top 5 hotspots.
- **Opportunity matrix**: only implement changes with Score ≥ 2.0 (Impact×Confidence/Effort).
- **Isomorphism proof**: prove ordering/tie-breaking/seed stability and record golden checksums.

### 3.12 Render-Trace + Checksum Format (v2)
This format powers deterministic replay + checksum verification for render regressions.

Version 2 replaces the v1 `true_color`/`colors_256` boolean pair with one
canonical `color_depth` value: `mono`, `ansi16`, `ansi256`, or `truecolor`.

#### Files
- `trace.jsonl`: one JSON object per line, strict key order (for deterministic diffs).
- `frames/`: optional binary payloads referenced by JSONL lines for replay.

#### Record Types
1. **Header** (`event="trace_header"`): run-wide metadata.
2. **Frame** (`event="frame"`): per-frame metadata + checksum + optional diff payload.
3. **Summary** (`event="trace_summary"`): final counters and checksum chain.

#### Required Header Fields
- `schema_version` (`"render-trace-v2"`)
- `run_id` (stable UUID/slug)
- `seed` (or `null`)
- `env` (os, arch, test_module)
- `capabilities` (terminal profile, canonical `color_depth`, and terminal features)
- `policies` (diff/bocpd/conformal toggles + key params)
- `start_ts_ms` (optional; excluded from checksums)

#### Required Frame Fields
- `frame_idx`
- `cols`, `rows`
- `mode` (`inline` | `inline_auto` | `alt`)
- `ui_height`, `ui_anchor`
- `diff_strategy` (`full` | `dirty` | `redraw`)
- `diff_cells`, `diff_runs`, `present_bytes`
- `render_us`, `present_us` (optional but recommended)
- `checksum` (u64 hex, 16 chars)
- `checksum_chain` (u64 hex, 16 chars)
- `payload_kind` (`diff_runs_v1` | `full_buffer_v1` | `none`)
- `payload_path` (relative path under `frames/`, or `null`)

#### Required Summary Fields
- `total_frames`
- `final_checksum_chain`
- `elapsed_ms`

#### Checksum Algorithm
- **Primary**: FNV-1a 64-bit over the final buffer grid (row-major).
- Hash input is the canonical serialized cell content + style:
  - `content_kind` (u8)
  - `content` (u32 char or u16 length + bytes for grapheme)
  - `fg_rgba` (u32), `bg_rgba` (u32)
  - `attrs` (u32)
- Use existing buffer checksum helper where available; algorithm must match across crates.

#### Payload Encoding (`diff_runs_v1`)
Binary format (little-endian) for replay:
```
u16 width, u16 height, u32 run_count
for each run:
  u16 y, u16 x0, u16 x1
  for x in [x0..x1]:
    u8 content_kind
    if content_kind == 0: (space)
    if content_kind == 1: u32 char
    if content_kind == 2: u16 len + [u8; len] grapheme bytes
    if content_kind == 3: (continuation)
    u32 fg_rgba
    u32 bg_rgba
    u32 attrs
```
Notes:
- Continuation cells are emitted explicitly to preserve layout.
- Grapheme bytes are UTF-8; `len` must be ≤ 4096 (hard cap).

#### Normalization Rules (for determinism)
- JSONL records must use stable key ordering and no extra whitespace.
- `ts_ms` fields are optional and ignored in checksum computation.
- Normalize line endings in any text payloads to `\n`.
- Sync-output markers (`SYNC_BEGIN`/`SYNC_END`) are ignored for checksuming.

---

#### Workflow (Capture + Replay)
1. **Capture**: write `trace.jsonl` (and optional `frames/`) for the scenario.
2. **Record context**: include `schema_version`, `seed`, `policies`, and terminal
   capabilities in the header record.
3. **Verify**: recompute `checksum` and `checksum_chain` during replay.
4. **Fail fast**: any mismatch in `checksum_chain` or `final_checksum_chain`
   is a hard regression.
5. **Review**: attach `trace.jsonl` + `frames/` artifacts to CI for diffing.

### 3.13 Conformal Predictor for Frame-Time Risk
This spec defines a distribution-free, explainable predictor for frame-time risk.
It outputs an *upper bound* on frame time with formal coverage guarantees and
integrates cleanly with diff strategy selection and budget enforcement.

#### 3.13.1 Goals
- **Coverage guarantee**: For each bucket, `P(y_t <= U_t) >= 1 - alpha` under
  exchangeability, where `U_t` is the predicted upper bound.
- **Explainability**: Each prediction logs the calibration set size, quantile,
  and residual statistics in the evidence ledger.
- **Graceful degradation**: If data is sparse or drifting, fall back to broader
  buckets or to a conservative baseline.
- **Mode-aware**: Works for inline, inline-auto, and alt-screen modes.
- **Large-screen priority**: Buckets must isolate large resolutions so coverage
  is not diluted by small-screen behavior.

#### 3.13.2 Definitions
- `y_t`: observed frame time at frame `t` (render + present, in microseconds).
- `x_t`: features for frame `t` (cols, rows, diff_cells, diff_runs, bytes,
  mode, diff_strategy, ui_height, ui_anchor, etc.).
- `f(x_t)`: point predictor (cost model from bd-3e1t.1.1 or fallback EMA).
- `r_t = y_t - f(x_t)`: one-sided residual (only upper risk matters).

#### 3.13.3 Buckets (Mondrian Conformal)
We use *Mondrian* (bucketed) conformal to reduce heterogeneity.
Bucket key `b` is a tuple:
```
mode_bucket = {inline, inline_auto, alt}
diff_bucket = {full, dirty, redraw}
size_bucket = floor(log2(cols * rows))   # isolates large screens
b = (mode_bucket, diff_bucket, size_bucket)
```
Fallback hierarchy if `n_b < min_samples`:
1. `(mode_bucket, diff_bucket, any_size)`
2. `(mode_bucket, any_diff, any_size)`
3. `global`

#### 3.13.4 Prediction Rule (One-Sided)
Given calibration residuals `{r_i}` in bucket `b` with size `n_b`:
```
q_b = quantile_{k}({r_i}),  k = ceil((n_b + 1) * (1 - alpha)) / n_b
U_t = f(x_t) + max(0, q_b)
```
Coverage guarantee (exchangeability within bucket):
```
P(y_t <= U_t) >= 1 - alpha
```
If `n_b < min_samples`, set `q_b = max(r_i)` from the fallback bucket, or
use a conservative constant `q_default` (documented in config).

#### 3.13.5 Calibration Window
We maintain a rolling window of residuals per bucket:
- `window_size` (default 256) with ring buffer semantics.
- For non-stationarity, BOCPD (bd-3e1t.2.*) may trigger **bucket reset** when a
  regime change is detected.
- Each reset is logged as a `conformal_reset` event with reason and evidence.

#### 3.13.6 Outputs
The predictor emits:
- `upper_us`: upper bound `U_t` for frame time.
- `risk`: boolean `upper_us > budget_us`.
- `confidence`: `1 - alpha` (coverage, not probability).
These outputs feed diff strategy selection and budget policy (bd-3e1t.3.3).

#### 3.13.7 Evidence Ledger (Required Fields)
Each prediction must log:
- `frame_idx`, `bucket_key`, `n_b`, `alpha`, `q_b`
- `y_hat` (`f(x_t)`), `upper_us`, `budget_us`, `risk`
- `fallback_level` (0..3), `window_size`, `reset_count`

#### 3.13.8 Tests (Required)
Unit tests:
- Quantile selection is correct for small `n_b` (edge cases).
- Fallback hierarchy behaves deterministically.
- One-sided coverage formula matches expected indices.

Integration tests:
- Bucket isolation for large-screen sizes (coverage not diluted).
- Reset on regime shift (BOCPD reset causes temporary conservative bounds).

#### 3.13.9 Runtime Defaults + Config
`ConformalConfig` defaults in `ftui-runtime`:
- `alpha = 0.05`
- `min_samples = 20`
- `window_size = 256`
- `q_default = 10_000.0` (microseconds)

Enable with:
```rust
use ftui_runtime::{ConformalConfig, ProgramConfig};
let config = ProgramConfig::default().with_conformal_config(ConformalConfig::default());
```
Disable with `ProgramConfig::without_conformal()`.

The predictor returns a `ConformalPrediction` with fields:
`upper_us`, `risk`, `confidence`, `bucket`, `sample_count`, `quantile`,
`fallback_level`, `window_size`, `reset_count`, `y_hat`, `budget_us`.
If you emit JSONL evidence, serialize these fields verbatim.

E2E tests (with JSONL logging):
- Inline + alt-screen coverage checks across size buckets.
- Stress: repeated large-screen frames with injected latency spikes.

#### 3.13.9 Implementation Targets
Likely modules:
- Predictor core: `crates/ftui-runtime/src/conformal.rs` (new or existing module)
- Config: `crates/ftui-runtime/src/program.rs` (ProgramConfig)
- Ledger logging: shared sink (bd-3e1t.4.7)

### 3.14 Dirty-Span Tracking for Sparse Diffs
This spec defines a per-row **dirty span** model used to avoid scanning full
rows when only small contiguous ranges have changed.

#### 3.14.1 Goals
- Preserve exact diff output (bit-identical to full scan).
- Reduce scan cost for sparse edits on large rows.
- Keep overhead < 2% in dense cases.
- Maintain determinism and explainability via evidence logs.

#### 3.14.2 Data Model
Each row `y` maintains an ordered list of half-open spans:
```
Span = { x0: u16, x1: u16 }  // represents [x0, x1), x0 < x1
RowSpans[y] = Vec<Span> sorted by x0, non-overlapping, non-adjacent
```
Row state is:
```
RowState = { spans: Vec<Span>, overflow: bool }
```
If `overflow == true`, the row must be scanned fully and `spans` is ignored.

#### 3.14.3 Invariants
For each row `y`:
- **Soundness**: if any cell `(x, y)` changed since last clear, then either:
  - `overflow == true`, or
  - there exists a span `s` with `s.x0 <= x < s.x1`.
- **Order**: spans are strictly increasing by `x0`.
- **Disjointness**: spans do not overlap or touch (`prev.x1 < next.x0`).
- **Bounds**: `0 <= x0 < x1 <= width`.

#### 3.14.4 Merge Policy (Interval Union)
When marking a change range `[a, b)` in row `y`:
1. Clamp to `[0, width)`; ignore if empty.
2. If `overflow == true`, stop (row already full-scan).
3. Find insertion point by `x0`.
4. Merge with any span that overlaps or is adjacent:
   - Overlap: `a <= s.x1 && b >= s.x0`
   - Adjacency: `a == s.x1` or `b == s.x0`
5. Replace the merged region with a single span `[min_x0, max_x1)`.
6. Maintain sorted, disjoint list.

This guarantees a minimal union of dirty intervals and deterministic ordering.

#### 3.14.5 Overflow / Fallback Policy
To bound memory and merge cost:
- Each row has `MAX_SPANS_PER_ROW` (e.g., 64).
- If inserting/merging would exceed the cap:
  - set `overflow = true`
  - clear spans (optional)
  - future marks are ignored (full-row scan).

Overflow reasons are logged (`cap_exceeded`, `span_merge_cost`, etc.).

#### 3.14.6 Scan Order (Deterministic)
When diffing with spans:
1. Iterate rows `y = 0..height`.
2. If `overflow == true`, scan full row left-to-right.
3. Else scan spans in ascending `x0`, and within each span scan `x` increasing.

This order is identical to the current row-major change list and ensures
bit-for-bit stable diff output.

#### 3.14.7 Evidence Ledger Fields
Each diff decision that uses spans must emit:
- `span_rows`: count of rows with non-empty spans
- `span_count`: total spans across all rows
- `span_coverage`: fraction of cells covered by spans
- `span_overflow_rows`: rows forced to full scan
- `fallback_reason`: `none` | `cap_exceeded` | `disabled` | `dense_override`
- `scan_cost_estimate`: expected cells scanned (spans + overflow rows)

#### 3.14.8 Tests (Required)
Unit tests:
- Merge/adjacency behavior (overlap, touch, disjoint).
- Overflow triggers and behavior.
- Deterministic ordering of spans after random insertions.

Property tests:
- Soundness: every changed cell is covered by a span or overflow.
- Equivalence: diff output with spans equals full scan on random buffers.

Benchmarks:
- Sparse (<= 5%) edits on 200x60+ show measurable improvement.
- Dense edits show < 2% overhead vs full scan.

---

## 4) Notes for Contributors

- The goal is not “perfect formalism”; the goal is to prevent drift.
- If you change behavior in Buffer/Presenter/TerminalSession, update this document and add tests.
