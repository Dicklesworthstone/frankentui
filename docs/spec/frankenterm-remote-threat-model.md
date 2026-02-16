# FrankenTerm Remote Attach Threat Model

**Status**: DRAFT  
**Bead**: `bd-2vr05.11.1`  
**Scope**: Browser embedding (`frankenterm-web`) + remote attach transport (`frankenterm-ws-v1`)

## 1. Purpose

Define explicit security boundaries, attack scenarios, mitigation ownership, and
deterministic incident-triage evidence requirements for remote attach and web embedding.

This document is implementation-oriented: each mitigation is mapped to concrete code,
protocol constraints, and tests/scripts.

## 2. Security Objectives

1. Prevent unauthorized PTY/session access.
2. Keep host/browser memory bounded under adversarial or pathological traffic.
3. Ensure link/clipboard behavior is safe by default.
4. Preserve deterministic, replayable evidence for post-incident triage.

## 3. Trust Boundaries

1. Browser host app ↔ FrankenTermWeb WASM (`FrankenTermWeb` JS API).
2. Browser client ↔ WebSocket bridge (`frankenterm-ws-v1`).
3. WebSocket bridge ↔ PTY/session runtime.

Untrusted inputs:

- All inbound websocket frames.
- All browser input events from host wiring.
- Clipboard payloads passed through host APIs.
- Hyperlink URLs parsed from terminal output.

## 4. Secure Defaults and Policy Surface

### 4.1 Link Policy Defaults

`LinkOpenPolicy` defaults are intentionally conservative:

- `allowHttp = false`
- `allowHttps = true`
- `allowedHosts = []`
- `blockedHosts = []`

Implementation:

- `crates/frankenterm-web/src/wasm.rs` (`LinkOpenPolicy::default`, `evaluate`, `setLinkOpenPolicy`)

Validation:

- `link_open_policy_defaults_to_https_only`
- `link_open_policy_snapshot_exposes_secure_defaults`
- `link_open_policy_blocks_http_when_disabled`

### 4.2 Clipboard Policy Defaults

- `clipboardPolicy()` defaults:
  - `copyEnabled = true`
  - `pasteEnabled = true`
  - `maxPasteBytes = 786432`
  - `hostManagedClipboard = true`
- No direct clipboard write side effect inside WASM:
  `copySelection()` returns text; host performs clipboard writes.
- Paste requires explicit host call (`pasteText`) and enforces max payload:
  `MAX_PASTE_BYTES = 786432` bytes decoded UTF-8.
- Hosts may further tighten policy using `setClipboardPolicy(...)`.

Implementation:

- `crates/frankenterm-web/src/wasm.rs` (`clipboardPolicy`, `setClipboardPolicy`, `copySelection`, `pasteText`)
- `docs/spec/frankenterm-websocket-protocol.md` (`Clipboard` message size limits)

Validation:

- `clipboard_policy_snapshot_exposes_secure_defaults`
- `set_clipboard_policy_can_disable_copy_and_paste`
- `paste_text_rejects_payload_above_max_bytes`
- `paste_text_respects_clipboard_policy_max_bytes_override`
- `extract_and_copy_selection_insert_row_breaks_at_grid_boundaries`

## 5. Bounded-Resource Contract

### 5.1 Host-Drained Queues (drop-oldest)

- `encoded_inputs_queue_max = 4096`
- `encoded_input_bytes_queue_max = 4096`
- `link_click_queue_max = 2048`
- `accessibility_announcement_queue_max = 64`
- `attach_transition_queue_max = 512`

Implementation:

- `crates/frankenterm-web/src/lib.rs` (`FRANKENTERM_JS_EVENT_BUFFER_POLICY`)
- `crates/frankenterm-web/src/wasm.rs` (`push_bounded`)
- `crates/frankenterm-web/src/attach.rs` (`TRANSITION_LOG_CAPACITY`)

Validation:

- `encoded_input_queues_drop_oldest_on_overflow`
- `link_click_queue_drops_oldest_on_overflow`
- `accessibility_announcement_queue_stays_bounded`
- `transition_log_is_bounded_to_capacity`

### 5.2 Protocol-Level Bounds

- Max ws envelope payload: `16 MiB` (`len` is 3-byte BE field).
- Clipboard message max: `1 MiB` base64 (`768 KiB` decoded).
- Flow-control windows are explicit and replenished deterministically.

Specification:

- `docs/spec/frankenterm-websocket-protocol.md` Sections 1.3, 2.11, 4

## 6. Threat Matrix (Attach + Embedding)

| Threat | Boundary | Mitigation | Evidence / Tests |
|---|---|---|---|
| Unauthorized remote attach | Browser ↔ WS bridge | Auth token/cookie required pre-PTY spawn; reject with `auth_failed` | Protocol spec §3.1; remote E2E auth paths |
| Cross-origin WS hijack | Browser ↔ WS bridge | Strict origin allow-list; reject missing/disallowed origin | Protocol spec §3.2; bridge logs rejected origins |
| Payload/queue memory exhaustion | Host/WASM and WS transport | Hard payload limits + bounded queues + drop-oldest policy | Queue bound tests; protocol size limits; flow-control diagnostics |
| Attach lifecycle desync/retry storms | WASM attach state machine | Deterministic state machine, bounded transition log, capped retries/backoff | `attach.rs` tests (`handshake_timeout...`, `transition_log_is_bounded_to_capacity`) |
| Unsafe link opening | Host/WASM | HTTPS-only default, explicit allow/block lists, reason-coded deny path + audit URL redaction fields | Link-policy unit tests + `drainLinkClicksJsonl` evidence |
| Clipboard abuse | Host/WASM and protocol | Host-gesture clipboard model, bounded payload, no command-exec API | `pasteText` max-size tests; protocol command-exec prohibition |
| Incident triage blind spots | All boundaries | JSONL evidence surfaces + deterministic replay fixtures | `drainAttachTransitionsJsonl`, `drainLinkClicksJsonl`, remote replay tests |

## 7. Deterministic Incident-Triage Workflow (Runbook)

### 7.1 Evidence Capture Checklist (always deterministic)

1. Capture a dedicated run with fixed seed/time-step:
   - `E2E_DETERMINISTIC=1`
   - `E2E_SEED=<incident-seed>`
2. Export attach/link evidence from the WASM surface:
   - `drainAttachTransitionsJsonl(run_id)`
   - `drainLinkClicksJsonl(run_id, seed, timestamp)`
3. Capture bridge/session telemetry:
   - `ws_bridge_telemetry.jsonl`
   - remote script summaries (`*_summary.json`)
4. Persist an incident bundle directory containing:
   - E2E JSONL
   - fixture JSONL (if applicable)
   - summary JSON
   - bridge telemetry JSONL
   - command line + seed metadata

### 7.2 Standard Triage Commands

```bash
# Full remote workflow smoke/regression evidence
rch exec -- /data/projects/frankentui/tests/e2e/scripts/test_remote_all.sh

# Deterministic attach ordering + lifecycle evidence
rch exec -- /data/projects/frankentui/tests/e2e/scripts/test_frankenterm_event_ordering_contract.sh

# Strict schema validation for replay-grade logs
python3 tests/e2e/lib/validate_jsonl.py <e2e-jsonl> \
  --schema tests/e2e/lib/e2e_jsonl_schema.json \
  --registry tests/e2e/lib/e2e_hash_registry.json \
  --strict
```

### 7.3 Incident Playbooks

| Incident Class | Signals | Immediate Containment | Deterministic Reproduction |
|---|---|---|---|
| Attach flapping / reconnect storm | Repeated `backing_off` / `retry_timer_elapsed`; rising reconnect counters | Keep retry caps/backoff enabled; reject parallel stale attaches; avoid disabling bounds in production | Re-run `test_frankenterm_event_ordering_contract.sh` with incident seed and compare transition ordering/action sets |
| Malformed-frame protocol desync | `protocol_error`, close codes (`1002/1008`), handshake failures | Fail closed on invalid envelopes; keep strict frame-length/type validation; close transport on fatal paths | Replay with fixed seed and inspect attach transition JSONL for deterministic `failure_code` + state progression |
| Output flood / memory pressure | Queue/window saturation, flow-control drop/coalesce decisions, high `bytes_rx` bursts | Preserve hard queue caps and output windows; prefer bounded drop/coalesce over buffer growth | Re-run remote stress scripts and compare JSONL metrics (`ws_metrics`, transition logs, summary artifacts) |
| Link/clipboard policy bypass attempt | Unexpected link opens, denied/accepted mismatch, oversize paste paths | Keep HTTPS-only + allow/block host gates; enforce paste max bytes and host-managed clipboard | Re-run remote paste/selection/link scripts and confirm policy reasons are stable in JSONL traces |

### 7.4 Documentation and Follow-Up

After mitigation:

1. Add/update risk entries in `docs/risk-register.md`.
2. Record the incident runbook delta in this threat model.
3. Attach incident bundle paths in bead/mail thread for replayable auditability.

## 8. Required E2E Evidence Paths

- `tests/e2e/scripts/test_remote_all.sh`
- `tests/e2e/scripts/test_remote_resize_storm.sh`
- `tests/e2e/scripts/test_remote_paste.sh`
- `tests/e2e/scripts/test_remote_selection_copy.sh`
- `tests/e2e/scripts/test_frankenterm_event_ordering_contract.sh`

These runs must emit structured JSONL suitable for deterministic replay and
postmortem correlation.

## 9. Open Risks

1. Origin/auth enforcement correctness still depends on bridge deployment config.
2. Token theft/session fixation defenses require server-side rollout discipline.
3. Link policy allow/block lists are host-configurable and can be weakened by integrators.
4. Large-scale adversarial traffic still requires bridge-side rate-limit tuning per deployment.
