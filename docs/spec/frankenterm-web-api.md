# FrankenTerm Web API Contract (`FrankenTermWeb`)

**Status**: DRAFT (contracted for stability)  
**Beads**: `bd-2vr05.9.1`, `bd-2vr05.13.1`  
**Audience**: SDK maintainers, integrators, migration authors, test owners

## Purpose

This document defines the stable public browser API for `FrankenTermWeb` and
its versioning policy. It is the canonical contract that host applications
should depend on instead of reverse-engineering implementation details.

This contract is intentionally independent from Rust crate semver:

- Rust package semver (`frankenterm-web` crate) controls Rust dependency update semantics.
- JS API semver (`apiVersion()`) controls browser SDK compatibility semantics.

## Contract Identity

- `apiLine`: `frankenterm-js`
- `apiVersion`: `1.0.0`
- `protocolVersion`: `frankenterm-ws-v1`

These values are exposed at runtime through:

- `FrankenTermWeb.apiVersion(): string`
- `FrankenTermWeb.apiContract(): { ... }`

`apiContract()` returns:

```json
{
  "apiLine": "frankenterm-js",
  "apiVersion": "1.0.0",
  "packageName": "frankenterm-web",
  "packageVersion": "0.1.0",
  "protocolVersion": "frankenterm-ws-v1",
  "eventSchemaVersion": "1.0.0",
  "eventTypes": ["...stable event taxonomy..."],
  "eventOrdering": ["...ordering guarantees..."],
  "eventBufferPolicy": ["...bounded queue policy..."],
  "methods": ["...stable JS method names..."],
  "versioningPolicy": ["major: ...", "minor: ...", "patch: ..."]
}
```

## Versioning Policy

The public browser contract uses semantic versioning:

- `major`: breaking change to public JS method names, argument semantics,
  return-shape fields, or event ordering.
- `minor`: additive, backwards-compatible expansion (new optional fields,
  methods, or options).
- `patch`: bugfix/performance/internal changes with no public contract change.

### Stability Guarantees

- Existing method names in `methods[]` remain callable for the same major line.
- Existing return object keys remain stable for the same major line.
- Event ordering and deterministic logging semantics remain stable for the same major line.
- New functionality is additive unless major is bumped.

### Non-Goals

- Implicit internal state behavior is not part of the contract unless documented here.
- Rust internals/layout are not a browser compatibility surface.

## Stable Method Surface (`1.0.0`)

### Lifecycle and rendering

- `init`
- `rendererBackend`
- `resize`
- `setScale`
- `setZoom`
- `fitToContainer`
- `render`
- `destroy`

### Input and transport

- `input`
- `drainEncodedInputs`
- `drainEncodedInputBytes`
- `drainImeCompositionJsonl`
- `createEventSubscription`
- `eventSubscriptionState`
- `drainEventSubscription`
- `drainEventSubscriptionJsonl`
- `closeEventSubscription`
- `imeState`
- `drainReplyBytes`
- `feed`
- `pasteText`

### Attach state machine

- `attachState`
- `attachConnect`
- `attachTransportOpened`
- `attachHandshakeAck`
- `attachTransportClosed`
- `attachProtocolError`
- `attachSessionEnded`
- `attachClose`
- `attachTick`
- `attachReset`
- `drainAttachTransitionsJsonl`

### Patch and viewport

- `applyPatch`
- `applyPatchBatch`
- `applyPatchBatchFlat`
- `viewportState`
- `viewportLines`
- `snapshotScrollbackFrameJsonl`
- `scrollLines`
- `scrollPages`
- `scrollToBottom`
- `scrollToTop`
- `scrollToLine`

### Search, selection, links

- `setSearchQuery`
- `searchNext`
- `searchPrev`
- `searchState`
- `clearSearch`
- `setSelectionRange`
- `clearSelection`
- `extractSelectionText`
- `clipboardPolicy`
- `copySelection`
- `setClipboardPolicy`
- `linkAt`
- `linkUrlAt`
- `drainLinkClicks`
- `drainLinkClicksJsonl`
- `setLinkOpenPolicy`
- `linkOpenPolicy`
- `setHoveredLinkId`

### Accessibility and shaping

- `setAccessibility`
- `accessibilityState`
- `accessibilityDomSnapshot`
- `accessibilityClassNames`
- `drainAccessibilityAnnouncements`
- `screenReaderMirrorText`
- `setCursor`
- `setTextShaping`
- `textShapingState`

### Contract introspection

- `apiVersion`
- `apiContract`
- `snapshotResizeStormFrameJsonl`

## Host Event Taxonomy (`eventSchemaVersion = 1.0.0`)

Canonical host-observable event classes:

- `attach.transition`
- `input.accessibility`
- `input.composition`
- `input.composition_trace`
- `input.focus`
- `input.key`
- `input.mouse`
- `input.paste`
- `input.touch`
- `input.vt_bytes`
- `input.wheel`
- `terminal.progress`
- `terminal.reply_bytes`
- `ui.accessibility_announcement`
- `ui.link_click`

### Progress Signal Mapping (`terminal.progress`)

`terminal.progress` is emitted from `feed()` when OSC `9;4` control sequences
complete (BEL/ST terminator observed), using deterministic normalization:

- `OSC 9;4;0;...` → `state="remove"`, `value=0`
- `OSC 9;4;1;N` → `state="normal"`, `value=clamp(N,0..100)` (missing `N` defaults to `0`)
- `OSC 9;4;2;N` → `state="error"`, `value` uses last known value when `N` missing/`0`
- `OSC 9;4;3;...` → `state="indeterminate"`, `value` uses last known value
- `OSC 9;4;4;N` → `state="warning"`, `value` uses last known value when `N` missing/`0`

Malformed OSC `9;4` payloads emit deterministic rejection envelopes
(`accepted=false`, `reason=...`) for traceable host diagnostics.

### Ordering Contract

The runtime guarantees:

1. `input()` emits normalized events in rewrite order. If composition rewrite
   inserts a synthetic event, it is emitted before the primary event.
2. While composition is active, key events are dropped until end/cancel.
3. `drainEncodedInputs()` preserves FIFO order across `input()` calls.
4. `drainEncodedInputBytes()` preserves FIFO VT-byte chunk order aligned to the
   same `input()` ordering.
5. `feed()` emits `terminal.progress` records in completed parser-sequence
   order for recognized OSC `9;4` progress signals.
6. `drainImeCompositionJsonl()` preserves FIFO IME rewrite traces (including
   synthetic composition and dropped-key records).
7. `drainReplyBytes()` preserves FIFO terminal reply ordering generated by
   `feed()`.
8. `drainAttachTransitionsJsonl()` preserves state-machine transition order.
9. `drainLinkClicks()` and `drainAccessibilityAnnouncements()` preserve FIFO
   order.
10. `drainEventSubscription()` / `drainEventSubscriptionJsonl()` preserve
   per-subscription FIFO by globally monotonic `seq` for selected event types.

### Bounded Buffering Contract

To prevent unbounded memory growth, host-drained queues use drop-oldest policy:

- `encoded_inputs_queue_max=4096`
- `encoded_input_bytes_queue_max=4096`
- `ime_trace_queue_max=2048`
- `link_click_queue_max=2048`
- `accessibility_announcement_queue_max=64`
- `attach_transition_queue_max=512`
- `event_subscription_queue_default_max=512` (configurable up to `8192`)
- `event_subscription_registry_max=256`

Host integrations should drain these queues at least once per render tick.

### Security Defaults (Attach + Embedding)

Default host-facing security posture:

- Link open policy defaults to HTTPS-only:
  - `allowHttp=false`
  - `allowHttps=true`
  - `allowedHosts=[]`
  - `blockedHosts=[]`
- Clipboard writes are host-managed only:
  - `clipboardPolicy()` defaults:
    - `copyEnabled=true`
    - `pasteEnabled=true`
    - `maxPasteBytes=786432`
    - `hostManagedClipboard=true`
  - `copySelection()` returns text but does not write clipboard directly.
  - Hosts are expected to use trusted user gestures for clipboard APIs.
  - `setClipboardPolicy(...)` can tighten or disable copy/paste behavior per host policy.
- Clipboard/paste payload is bounded:
  - `pasteText()` rejects payloads over `786432` UTF-8 bytes.
- Link click drains include policy/audit fields for triage:
  - `policyRule` / `actionOutcome`
  - `auditUrl` / `auditUrlRedacted`

## Determinism and Logging Requirements

For fixed inputs and capability profile:

- `apiContract()` output is deterministic.
- Method list ordering is deterministic.
- JSONL-producing methods produce deterministic shapes and required fields.

Structured logging paths (current):

- `scripts/e2e_test.sh`
- `scripts/demo_showcase_e2e.sh`
- `tests/e2e/scripts/test_frankenterm_event_ordering_contract.sh`

These scripts are expected to preserve JSONL diagnostics required for replay and
triage, including run identifiers and timestamp context.

## Validation Requirements

Unit tests MUST enforce:

- API semver format validity.
- Stable method list ordering + uniqueness.
- Versioning policy presence (`major`, `minor`, `patch`).
- Event taxonomy ordering + uniqueness.
- Event ordering and bounded-buffer policy declarations are present.

E2E coverage MUST enforce:

- Browser host can call `apiContract()` and validate required fields.
- Host-observed drain ordering remains aligned with `apiContract().eventOrdering`
  under burst input, resize transitions, and attach mode transitions.
- Replay-oriented JSONL traces still parse after SDK changes.

## Migration Guidance (xterm.js → FrankenTermJS)

- Prefer capability probing via `apiContract()` over duck-typing individual methods.
- Pin to `apiLine + major(apiVersion)` in integration checks.
- Treat `minor` as safe additive upgrade.
- Treat `major` as migration-required and consult updated migration guide.

## References

- `docs/spec/frankenterm-websocket-protocol.md`
- `docs/spec/frankenterm-remote-threat-model.md`
- `docs/spec/wasm-showcase-runner-contract.md`
- `crates/frankenterm-web/src/lib.rs`
- `crates/frankenterm-web/src/wasm.rs`
