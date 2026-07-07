# FrankenTermJS SDK — API Reference and Usage Cookbook

> The practical reference for embedding a FrankenTermJS terminal
> (bd-2vr05.9.5). The **normative contract** is
> [`docs/spec/frankenterm-web-api.md`](spec/frankenterm-web-api.md); this
> document is the developer-facing companion: the typed surfaces at a glance,
> plus recipes for the integration cases adopters actually hit. If this file
> and the spec ever disagree, the spec wins and this file has a bug.

**In-tree vs out-of-tree.** The `frankenterm-web` WASM packaging crate is not
vendored in this repository. The *durable sources of truth* live in the
`ftui-web` crate and are what every table below is generated from:

| Surface | Source of truth | Lockstep guard |
|---------|-----------------|----------------|
| Typed events + errors | `crates/ftui-web/src/sdk_event_model.rs` | `frankenterm_js_sdk_contract_compat.rs` asserts the committed `sdk/frankenterm-js-events.d.ts` matches the generator |
| Adapter lifecycles + examples | `crates/ftui-web/src/sdk_adapter.rs` | lib tests assert `sdk/examples/*.{js,tsx}` match the generators |
| Runtime options + validation | `crates/ftui-web/src/runtime_options.rs` | `frankenterm_js_runtime_options_e2e.rs` cross-engine matrix |
| Contract identity + method surface | `docs/spec/frankenterm-web-api.md` | `frankenterm_js_sdk_validation_e2e.rs` + conformance CI gates |

Validation entry points: `scripts/frankenterm_js_sdk_adapter_e2e.sh` (adapter +
contract validation, JSONL evidence) and
`scripts/frankenterm_js_runtime_options_e2e.sh` (option matrix).

---

## 1. Contract identity

Pin the contract before any other call:

```js
const contract = FrankenTermWeb.apiContract();
if (contract.apiLine !== "frankenterm-js" || !String(contract.apiVersion).startsWith("1.")) {
  throw new Error(`unsupported FrankenTermWeb contract: ${contract.apiVersion}`);
}
```

- `apiVersion` follows the versioning policy in the spec: the `1.x` line only
  adds; it never renames or removes documented methods, event types, or error
  codes.
- `eventSchemaVersion` is `1.0.0` (mirrored by
  `sdk_event_model::EVENT_SCHEMA_VERSION`).
- Prefer capability probing via `apiContract()` over duck-typing individual
  methods (see the migration guide).

## 2. Typed host events (`eventSchemaVersion = 1.0.0`)

Fifteen event classes, drained (not pushed) by the host. Wire strings are
sorted and stable; TypeScript types ship in `sdk/frankenterm-js-events.d.ts`.

| Wire string | Namespace | Meaning |
|-------------|-----------|---------|
| `attach.transition` | `attach` | websocket attach state-machine transition |
| `input.accessibility` | `input` | assistive-technology originated input |
| `input.composition` | `input` | IME composition lifecycle |
| `input.composition_trace` | `input` | IME rewrite/diagnostic trace |
| `input.focus` | `input` | focus gained/lost |
| `input.key` | `input` | key press/release |
| `input.mouse` | `input` | mouse button/motion |
| `input.paste` | `input` | bracketed paste payload |
| `input.touch` | `input` | touch input |
| `input.vt_bytes` | `input` | raw VT bytes forwarded to the PTY |
| `input.wheel` | `input` | wheel/scroll input |
| `terminal.progress` | `terminal` | OSC 9;4 progress signal |
| `terminal.reply_bytes` | `terminal` | terminal reply bytes for the transport |
| `ui.accessibility_announcement` | `ui` | screen-reader announcement |
| `ui.link_click` | `ui` | hyperlink activation |

(`HostEventClass::ALL` is the authoritative list — the validation suite
asserts count, sorted order, and wire round-trips.)

**Drain-driven, not push-driven.** xterm.js pushes to `onData`-style handlers;
FrankenTermJS lets the host drain on its own schedule:

```js
setInterval(() => {
  for (const line of term.drainEventSubscriptionJsonl()) {
    const event = JSON.parse(line);
    // route by event.type using the taxonomy above
  }
}, 16);
```

## 3. Typed errors

### Engine errors (`SdkErrorKind`, 8 codes)

| Code | Meaning |
|------|---------|
| `attach.protocol_error` | websocket attach protocol fault |
| `capability.unsupported` | requested feature/renderer not advertised |
| `clipboard.disabled` | copy/paste disabled by host clipboard policy |
| `clipboard.paste_too_large` | paste exceeds `maxPasteBytes` |
| `input.parse` | malformed host-encoded input payload |
| `queue.overflow` | a bounded host-drained queue dropped its oldest entry |
| `terminal.progress.malformed` | malformed OSC 9;4 progress payload |
| `ui.link.blocked` | hyperlink activation denied by link-open policy |

### Adapter-layer misuse (`adapter.*`, 5 codes)

Lifecycle-ordering mistakes are a *different layer* from engine errors and use
a disjoint namespace (validated to never collide):

| Code | You called | Fix |
|------|-----------|-----|
| `adapter.not_mounted` | attach/resize before `init` | call `init` first |
| `adapter.not_attached` | input/detach without a transport | call `attachConnect` first |
| `adapter.double_mount` | `init` twice (vanilla) | dispose before re-mounting; React dedups this instead |
| `adapter.already_attached` | `attachConnect` twice (vanilla) | `attachClose` before reconnecting |
| `adapter.disposed` | anything after `destroy` | create a new adapter |

## 4. Bounded buffering (drop-oldest)

Every host-drained queue is bounded; overflow drops the oldest entry and
surfaces `queue.overflow`. Defaults (`EventBufferPolicy::DEFAULT`):

| Queue | Default max |
|-------|-------------|
| `encoded_inputs_queue_max` | 4096 |
| `encoded_input_bytes_queue_max` | 4096 |
| `ime_trace_queue_max` | 2048 |
| `link_click_queue_max` | 2048 |
| `accessibility_announcement_queue_max` | 64 |
| `attach_transition_queue_max` | 512 |
| `event_subscription_queue_default_max` | 512 (configurable up to 8192) |
| `event_subscription_queue_configurable_max` | 8192 |
| `event_subscription_registry_max` | 256 |

Rule of thumb: drain at frame cadence (16 ms) and the defaults are generous;
if you drain lazily, raise the subscription queue max at creation instead of
accepting drops.

## 5. Runtime options

`RuntimeOptions` (validated atomically against the engine's
`OptionCapabilityProfile`; a rejected patch changes nothing and reports every
offending field):

| Option | Default | Range / notes |
|--------|---------|---------------|
| `cursor_style` | `Block` | `Block` \| `Underline` \| `Bar` |
| `cursor_blink` | `false` | |
| `scrollback_lines` | `1000` | `0` disables scrollback |
| `tab_width` | `8` | `1..=16` |
| `convert_eol` | `false` | treat bare LF as CRLF on output |
| `screen_reader_mode` | `false` | assistive output mode |
| `bracketed_paste` | `true` | |
| `minimum_contrast_ratio_x100` | `100` | `100..=2100`; `100` disables enforcement |
| `renderer` | `Dom` | `Dom` \| `Canvas` \| `WebGl` \| `WebGpu`, gated by the capability profile |
| `theme` | dark palette | anchor colors must be fully opaque |

The default options validate on **every** capability profile (the boot
guarantee): a fresh terminal starts anywhere, and the host upgrades the
renderer via an explicit patch once it knows what the engine supports.

## 6. Adapter lifecycle (first-party adapters)

The recommended wiring is an executable model
(`sdk_adapter::AdapterLifecycle`), not prose. Phases and legal actions:

```text
Created ──Mount──▶ Mounted ──Attach──▶ Attached ──Detach──▶ Detached
   │                  │  ▲                │ ▲                  │ │
   │               Resize │            Resize/Input           │ Attach (reconnect)
   └──────Dispose─────┴───┴──────Dispose──┴────────Dispose────┴─▶ Disposed
```

- `Resize` is legal from `Mounted` onward (`fitToContainer` runs between
  `init` and `attachConnect`); `Input` requires `Attached`.
- `Dispose` (`destroy`) is reachable from every live phase; teardown order is
  always `attachClose` **then** `destroy`.
- **React/Next semantics:** StrictMode double-invokes effects in development.
  The React adapter dedups repeated idempotent steps (mount when mounted,
  detach when detached, dispose when disposed) as `strict_mode_deduped`
  outcomes; the vanilla adapter reports the same repeats as `adapter.*`
  misuse. Reconnect flows (`Detached → Attach`) are legal for both.
- Every applied action and every rejected misuse yields a deterministic,
  timestamp-free JSONL line carrying `seq` + `adapter_id` for correlation
  (harness layers add wall-clock timestamps).

## 7. Cookbook

### R1 — Embed in a vanilla page

Use the canonical example verbatim:
[`crates/ftui-web/sdk/examples/frankenterm-adapter-vanilla.js`](../crates/ftui-web/sdk/examples/frankenterm-adapter-vanilla.js).
It pins the contract, mounts, sizes, attaches, wires input + draining, and
returns a `dispose()` that tears down in the correct order. Vanilla hosts must
call `dispose()` exactly once.

### R2 — Embed in React / Next.js

Use
[`crates/ftui-web/sdk/examples/frankenterm-adapter-react.tsx`](../crates/ftui-web/sdk/examples/frankenterm-adapter-react.tsx).
The three React-specific rules, all encoded in the example:

1. `"use client"` + a `typeof window` guard — engine code never runs in SSR.
2. One effect owns the whole lifecycle; the cleanup *is* the teardown.
3. StrictMode double-invocation is expected — every step is
   idempotent-or-deduped by the adapter, so don't add ref-counting hacks.

### R3 — Resizing

Call `fitToContainer()` once after `init`, then let a `ResizeObserver` drive
subsequent sizing. If you manage the grid yourself, call `resize(cols, rows)`
with the numbers you computed — never both patterns at once.

### R4 — Routing typed events

Parse each drained JSONL line and switch on `event.type` against the taxonomy
table (§2). Unknown types are forward-compatible additions within `1.x`:
ignore them, don't throw.

### R5 — Error handling in two layers

Engine errors arrive with dotted codes from §3; adapter misuse means *your
wiring order* is wrong (fix the code path, don't retry). Log both into the
same timeline keyed by your adapter id — the misuse explanations name the
recommended fix.

### R6 — Patching options at runtime

Apply sparse patches; validation is atomic. On rejection you get every
offending field (e.g. `capability.unsupported` for an ungated renderer), and
the terminal keeps running with its previous options. Probe
`apiContract()`/the capability profile before offering renderer choices in
your UI.

### R7 — Reconnect flows

`attachClose()` then `attachConnect(url)` — the adapter model keeps the
engine mounted through `Detached`, so scrollback and options survive
transport bounces. Don't destroy/rebuild the engine to reconnect.

### R8 — Teardown correctness

Detach before destroy, always. If teardown can run twice (React cleanup,
error paths), either route it through the React adapter semantics or guard
your vanilla `dispose()` — `adapter.disposed` on a second dispose is telling
you the host called it twice.

## 8. Migration from xterm.js

Read [`docs/migration/xterm-js-to-frankenterm-js.md`](migration/xterm-js-to-frankenterm-js.md)
— method/addon mapping, intentional divergences (drain-driven events being
the big one), framework wiring, and troubleshooting.

## 9. How these docs stay honest

- The event/error tables mirror `HostEventClass::ALL` / `SdkErrorKind::ALL`,
  whose order, count, and round-trips are asserted by
  `crates/ftui-web/tests/frankenterm_js_sdk_validation_e2e.rs`.
- The committed `.d.ts` and both adapter examples are generator-locked
  (byte-identical) by lib tests.
- `scripts/frankenterm_js_sdk_adapter_e2e.sh` runs the full validation lane
  and archives `FTUI_SDK_ADAPTER_COMPAT` JSONL evidence per run.
