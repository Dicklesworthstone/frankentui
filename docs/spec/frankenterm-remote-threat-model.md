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

- No direct clipboard write side effect inside WASM:
  `copySelection()` returns text; host performs clipboard writes.
- Paste requires explicit host call (`pasteText`) and enforces max payload:
  `MAX_PASTE_BYTES = 786432` bytes decoded UTF-8.

Implementation:

- `crates/frankenterm-web/src/wasm.rs` (`copySelection`, `pasteText`)
- `docs/spec/frankenterm-websocket-protocol.md` (`Clipboard` message size limits)

Validation:

- `paste_text_rejects_payload_above_max_bytes`
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
| Unsafe link opening | Host/WASM | HTTPS-only default, explicit allow/block lists, reason-coded deny path | Link-policy unit tests + `drainLinkClicksJsonl` evidence |
| Clipboard abuse | Host/WASM and protocol | Host-gesture clipboard model, bounded payload, no command-exec API | `pasteText` max-size tests; protocol command-exec prohibition |
| Incident triage blind spots | All boundaries | JSONL evidence surfaces + deterministic replay fixtures | `drainAttachTransitionsJsonl`, `drainLinkClicksJsonl`, remote replay tests |

## 7. Deterministic Incident-Triage Workflow (Runbook)

When an attach/security incident occurs:

1. Capture JSONL evidence:
   - `drainAttachTransitionsJsonl(run_id)`
   - `drainLinkClicksJsonl(run_id, seed, timestamp)`
   - Remote E2E logs from scripts below
2. Classify failure domain:
   - auth/origin/rate-limit/payload-bounds/link-policy/clipboard
3. Reproduce deterministically:
   - replay same input/event sequence and seed
   - verify transition/order invariants
4. Contain safely:
   - prefer false-block for auth/origin checks
   - prefer bounded degradation (drop/coalesce/throttle) over unbounded growth
5. Document:
   - add/update risk entry in `docs/risk-register.md`
   - update this threat model with new mitigation/test coverage

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

