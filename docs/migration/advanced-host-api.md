# Advanced Host API Migration & Capability Mapping (xterm.js → FrankenTermJS)

> Migration guide for the **advanced** host API surface: event lifecycle, parser
> hooks, markers/decorations, and runtime option/theming mutation. For the base
> terminal contract see [`docs/spec/frankenterm-web-api.md`](../spec/frankenterm-web-api.md);
> for the overall synthesis philosophy see [`docs/migration-map.md`](../migration-map.md).

This guide covers the four subsystems delivered by the advanced host API parity
track (feature `bd-2vr05.13`). It maps each xterm.js concept to its FrankenTermJS
equivalent, calls out **intentional divergences**, documents the capability flags
and security/determinism guarantees, and ties every behaviour back to the
structured logs emitted by the compatibility harness
(`scripts/frankenterm_js_advanced_api_compat.sh`, `bd-2vr05.13.6`).

## Where the code lives (and a transience caveat)

FrankenTermJS is **not a port**. The durable advanced-API implementation lives in
the `ftui-*` crates. The browser-facing `frankenterm-web` WASM package is a
*transient/extracted* crate that is repeatedly split out and folded back in, so
treat it as the JS-binding wrapper, not the source of truth:

| Subsystem | In-tree home (source of truth) |
|-----------|--------------------------------|
| Event lifecycle / ordering | `ftui-web` input pipeline + the `FrankenTermWeb` contract in [`frankenterm-web-api.md`](../spec/frankenterm-web-api.md) |
| Parser hooks (CSI/OSC/DCS/ESC) | `crates/ftui-extras/src/terminal/parser.rs` |
| Markers / viewport anchors | `crates/ftui-web/src/step_program.rs` (baseline-reset markers) |
| Runtime options / theming | `crates/ftui-web/src/runtime_options.rs` |

---

## 1. Event lifecycle & ordering

xterm.js exposes events as `IEvent` emitters (`onData`, `onKey`, `onResize`,
`onRender`, `onBell`, …) with no formal ordering or backpressure contract.
FrankenTermJS replaces ad-hoc emitters with a **deterministic, bounded, drainable**
taxonomy (`eventSchemaVersion = 1.0.0`).

| xterm.js | FrankenTermJS | Divergence |
|----------|---------------|------------|
| `onData(cb)` | `input.key` / `input.vt_bytes` (drained via `drainEncodedInputs()` / `drainEncodedInputBytes()`) | Host pulls FIFO queues; no push callbacks |
| `onKey(cb)` | `input.key` | Suppressed while IME composition is active |
| `onResize(cb)` | `attach.transition` + geometry markers (§3) | Resize forces a deterministic baseline reset |
| `onRender(cb)` | host renders from drained patches | Rendering is host-driven, not event-driven |
| `onTitleChange` / `onBell` | `terminal.reply_bytes` / OSC handling | Folded into the reply/feed stream |
| (none) | `terminal.progress` (OSC `9;4`) | First-class progress signal mapping |

**Ordering contract** (see spec §"Ordering Contract"): composition rewrites emit
synthetic events before the primary event; key events drop while composition is
active; every `drain*()` method preserves FIFO, and `drainEventSubscription*()`
preserves per-subscription FIFO by a globally monotonic `seq`.

**Bounded buffering**: every drained queue uses drop-oldest with an explicit cap
(`encoded_inputs_queue_max=4096`, `ime_trace_queue_max=2048`,
`accessibility_announcement_queue_max=64`, …). Host integrations **must drain at
least once per render tick**. There is no xterm.js equivalent — unbounded
listeners are the default there.

---

## 2. Parser hooks (CSI / OSC / DCS / ESC)

xterm.js `parser.registerCsiHandler` / `registerOscHandler` /
`registerDcsHandler` / `registerEscHandler` return a disposable and run handlers
in an unspecified order with no quotas, capability gating, or fault isolation.

FrankenTermJS — `ftui_extras::terminal::parser::AnsiParser`:

```rust
let id = parser.register_csi_hook(|e: &CsiHookEvent| HookDisposition::Consume);
parser.register_osc_hook(|e: &OscHookEvent| HookDisposition::Continue);
parser.register_esc_hook(|e: &EscHookEvent| HookDisposition::Continue);
parser.register_dcs_hook(|e: &DcsHookEvent| HookDisposition::Continue);
parser.deregister_hook(id);
parser.set_hook_policy(HookPolicy { max_csi_invocations_per_parse: 64, ..Default::default() });
parser.set_hook_capabilities(HookCapabilities { dcs: false, ..Default::default() });
let trace: Vec<HookTraceEvent> = parser.drain_hook_trace();
```

| xterm.js | FrankenTermJS | Divergence |
|----------|---------------|------------|
| `registerCsiHandler(id, cb)` | `register_csi_hook(cb) -> HookId` | Hooks dispatch in **registration order** (deterministic) |
| handler returns `bool` (handled) | `HookDisposition::{Continue, Consume, Reject}` | Explicit fallthrough vs. consume vs. reject |
| `dispose()` | `deregister_hook(id) -> bool` | — |
| (none) | `set_hook_policy(HookPolicy)` | Per-`parse()` invocation **quotas** + callback **timeout** |
| (none) | `set_hook_capabilities(HookCapabilities)` | Per-class **capability gating** (CSI/OSC/ESC/DCS) |
| (none) | `drain_hook_trace() -> Vec<HookTraceEvent>` | Structured, replayable hook trace |

**Fault isolation** (no xterm.js analogue): a hook that **panics** is caught and
recorded as `HookRejectReason::HookPanicked`; a hook that exceeds
`HookPolicy::max_hook_runtime` is recorded as `TimeoutExceeded`; in both cases the
parser falls back to the terminal handler rather than aborting. A disabled class
yields `CapabilityDisabled`; an over-quota dispatch yields `QuotaExceeded`. Every
outcome lands in the trace as a `HookTraceStage` (`HookInvoked`, `HookConsumed`,
`FallbackDispatched`, `PolicyRejected`) with a monotonic `correlation_id`.

---

## 3. Markers & decorations (viewport-stable anchors)

xterm.js `registerMarker(cursorYOffset)` returns an `IMarker` that tracks a buffer
line, and `registerDecoration(options)` attaches a decoration to a marker. The
full marker/decoration **object** API is part of the `frankenterm-web` package
surface. The in-tree, deterministic primitive that keeps host decorations
**viewport-stable across reflow** is the geometry-transition baseline marker
emitted by `StepProgram`:

On any geometry transition (resize, host fit, DPR/zoom — even when cols/rows are
numerically unchanged), the runtime invalidates the diff baseline and emits two
structured log markers before the next full repaint:

```json
{"event":"diff_baseline_reset","reason":"geometry_transition","from_cols":80,"from_rows":24,"to_cols":120,"to_rows":40,"frame_idx":7}
{"event":"full_repaint_boundary","reason":"geometry_transition","from_cols":80,"from_rows":24,"to_cols":120,"to_rows":40,"frame_idx":7,"full_repaint":true}
```

| xterm.js | FrankenTermJS | Divergence |
|----------|---------------|------------|
| `registerMarker(y)` → `IMarker` | host anchors against `diff_baseline_reset` markers | Anchors re-projected at deterministic repaint boundaries |
| `marker.onDispose` | baseline reset on transition | Reflow is an explicit, logged boundary |
| `registerDecoration(opts)` | host-side decoration keyed to anchors | Decoration objects live in the JS wrapper |

A host re-projects its decorations whenever a `full_repaint_boundary` marker
appears; between transitions, anchors are stable. A plain tick emits **no** marker
(the signal is never spuriously raised).

---

## 4. Runtime options & theming

xterm.js mutates `term.options.X = value` field-by-field with immediate,
non-atomic effect and silent clamping. FrankenTermJS replaces this with an
**atomic, validated, capability-gated** patch API — `ftui_web::runtime_options`:

```rust
let mut opts = RuntimeOptions::default();
let caps = OptionCapabilityProfile::full();
let patch = RuntimeOptionPatch::parse_json(
    r#"{"cursorStyle":"bar","scrollback":5000,"rendererType":"webgpu"}"#
)?;                                   // parse errors: shape/type/unknown-key
let update = opts.apply_patch(&patch, &caps, "corr-id");  // validation + gating
assert!(update.applied);              // all-or-nothing; rolls back on any error
```

| xterm.js `term.options` | FrankenTermJS JSON patch key | Type / values |
|-------------------------|------------------------------|---------------|
| `cursorStyle` | `cursorStyle` | `"block" \| "underline" \| "bar"` |
| `cursorBlink` | `cursorBlink` | bool |
| `scrollback` | `scrollback` | `0..=1_000_000` (hard max), gated by host budget |
| `tabStopWidth` | `tabStopWidth` | `1..=16` |
| `convertEol` | `convertEol` | bool |
| `screenReaderMode` | `screenReaderMode` | bool |
| (paste handling) | `bracketedPaste` | bool |
| `minimumContrastRatio` | `minimumContrastRatioX100` | `100..=2100` (×100 WCAG ratio) |
| renderer addon choice | `rendererType` | `"dom" \| "canvas" \| "webgl" \| "webgpu"`, capability-gated |
| `theme` | `theme` | `{foreground, background, cursor, cursorAccent, selectionBackground, ansi0..ansi15}` as `#rrggbb`/`#rrggbbaa` |

**Intentional divergences:**

- **Atomic apply + rollback.** `apply_patch` validates the *entire* candidate
  (schema range + capability gating), collects **every** error, and commits
  all-or-nothing. A single bad field leaves the options byte-for-byte unchanged —
  there is no partial application or silent clamp.
- **Two-stage error model.** Shape errors (`OptionPatchParseError`:
  malformed JSON / unknown key / type mismatch / invalid enum token) are surfaced
  at `parse_json`; semantic errors (`RuntimeOptionError`: `OutOfRange`,
  `TransparentAnchorColor`, `CapabilityGated`) at `apply_patch`.
- **Default renderer is `dom`.** The most compatible backend is the default so a
  fresh terminal validates on *every* engine; the host upgrades to
  canvas/webgl/webgpu via an explicit patch once it knows the engine's caps.
- **Theme anchors must be opaque.** `foreground`/`background` reject a non-opaque
  alpha (a transparent default fg/bg is meaningless for a grid); `selectionBackground`
  may be translucent.
- **Renderer/engine sync is explicit.** The returned `RuntimeOptionUpdate` reports
  `requires_repaint` and `requires_renderer_reinit` so the host performs exactly
  the work a change demands.

---

## 5. Capability flags & profile gating

A value can be *schema-valid* yet *unsupported by the active engine*. Gating is
explicit and distinct from range errors:

| Capability source | Gates |
|-------------------|-------|
| `OptionCapabilityProfile { dom, canvas, webgl, webgpu, color_depth, max_scrollback_lines }` | `rendererType` (must be advertised) and `scrollback` (≤ host budget) |
| `HookCapabilities { csi, osc, esc, dcs }` | parser hook classes (disabled class ⇒ `CapabilityDisabled`) |

Capability gating applies **only to otherwise schema-valid values**, so a single
field never double-reports both an out-of-range and a gating error. Prefer probing
the host's `apiContract()` (see spec) over duck-typing individual methods.

---

## 6. Security & bounded resources

| Concern | Control |
|---------|---------|
| Parser hook abuse | `HookPolicy` per-`parse()` invocation quotas + per-callback `max_hook_runtime` timeout; panic isolation |
| Unbounded event growth | Drop-oldest bounded queues (spec §"Bounded Buffering") |
| Scrollback blowup | `SCROLLBACK_HARD_MAX = 1_000_000` independent of host budget |
| Link opens | HTTPS-only default (`allowHttp=false`), host allow/block lists (spec §"Security Defaults") |
| Clipboard / paste | Host-managed clipboard, `maxPasteBytes=786432`, trusted-gesture model |

---

## 7. Determinism guarantees

For a fixed input and capability profile, every advanced-API surface is
deterministic: parser-hook traces use monotonic `correlation_id`s; option
`apply_patch` outcomes and their JSONL serialisation are byte-identical across
runs; geometry markers reproduce exactly; and all `drain*()` orderings are FIFO.
This is what makes the compatibility harness (§8) byte-for-byte reproducible.

---

## 8. Troubleshooting via structured logs

The compatibility harness aggregates every subsystem's evidence into one manifest:

```bash
./scripts/frankenterm_js_advanced_api_compat.sh         # release-blocking gate
# → <logdir>/advanced_api_compat_manifest.jsonl
# → <logdir>/advanced_api_compat_summary.json
```

Each manifest row carries `subsystem`, a global `manifest_seq`, `scenario`,
`case`, `correlation_id`, and `passed`, plus subsystem-specific fields:

| Subsystem | Evidence prefix | Key fields for triage |
|-----------|-----------------|------------------------|
| `parser_hooks` | `FTUI_ADVANCED_API_COMPAT` | `invoked`, `consumed`, `fallback`, `rejected`, `reject_reasons[]`, `classes[]` |
| `markers` | `FTUI_ADVANCED_API_COMPAT` | `baseline_reset`, `full_repaint_boundary`, `full_repaint_hint` |
| `runtime_options` | `FTUI_RUNTIME_OPTIONS_MATRIX` | `applied`, `change_count`, `error_count`, `requires_repaint`, `requires_renderer_reinit` |
| `events_ime` | `FTUI_A11Y_MATRIX` | composition phase stream, coalescing/downgrade counts |

A rejected runtime-option update serialises a full audit line:

```json
{"event":"runtime_option_update","correlation_id":"...","applied":false,"requires_repaint":false,"requires_renderer_reinit":false,"change_count":0,"error_count":1,"changes":[],"errors":[{"field":"renderer","kind":"capability_gated","detail":"option `renderer` renderer `webgpu` is not supported by the active host"}]}
```

**Common triage paths:**

- *"My option change did nothing."* Grep the update line for `"applied":false` and
  read `errors[].kind` — `out_of_range`, `transparent_anchor_color`, or
  `capability_gated` tells you whether it was nonsense or merely unsupported here.
- *"My parser hook never fired."* Check the hook trace for `PolicyRejected` with a
  `reject_reasons` of `capability_disabled` (class off), `quota_exceeded` (raise
  `HookPolicy`), `timeout_exceeded` (callback too slow), or `hook_panicked`.
- *"Decorations jumped after resize."* Confirm a `full_repaint_boundary` marker was
  emitted and that the host re-projected anchors at that `frame_idx`.

The harness `summary.json` records `missing_subsystems` and `failed_cells`; CI runs
it as a release-blocking gate in the `golden-trace-gates` job.

---

## References

- [`docs/spec/frankenterm-web-api.md`](../spec/frankenterm-web-api.md) — base `FrankenTermWeb` contract, event taxonomy, ordering, security defaults
- [`docs/migration-map.md`](../migration-map.md) — overall synthesis philosophy
- `crates/ftui-extras/src/terminal/parser.rs` — parser hook API
- `crates/ftui-web/src/runtime_options.rs` — runtime option/theming API
- `crates/ftui-web/src/step_program.rs` — geometry-transition baseline markers
- `scripts/frankenterm_js_advanced_api_compat.sh` — compatibility harness aggregator
