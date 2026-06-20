# FrankenTerm Conformance / Differential / Fuzz Release Gates

> Triage guide for the release-blocking quality gates wired by **bd-2vr05.10.5**.
> These gates are **mandatory**, not advisory: a red gate blocks the release.

The conformance/differential/fuzzing suites (bd-2vr05.10.1–.10.4) run as
release-blocking CI jobs. This page maps each gate to its runner, its structured
artifacts, and how to triage a failure.

## Gate matrix

| Gate | Bead | CI job / step | Runner | Structured artifacts |
|------|------|---------------|--------|----------------------|
| VT support-matrix conformance | 10.1 | `frankenterm-conformance-gates` → *Conformance* | `scripts/vt_support_matrix_e2e.sh` → `ftui-pty` `vt_support_matrix_runner` | `vt_support_matrix_results.jsonl`, `vt_support_matrix_summary.json` |
| xterm.js shared-fixture differential | 10.2 | `frankenterm-conformance-gates` → *Differential* | `tests/e2e/scripts/test_xterm_shared_fixture_differential.sh` | `xterm_shared_fixture_differential_*.jsonl`, `*_report.json`, `*_summary.json` |
| WebSocket protocol compliance | 10.3 | `frankenterm-conformance-gates` → *WebSocket* | `tests/e2e/scripts/test_ws_protocol_compliance.sh` (drives `ftui-pty` `frankenterm_ws_bridge`) | per-frame JSONL + compliance report |
| Adversarial security/reliability | 11.6 | `frankenterm-conformance-gates` → *Adversarial security/reliability* | `scripts/frankenterm_js_security_reliability_compat.sh` | `security_reliability_compat_manifest.jsonl`, `security_reliability_compat_summary.json` |
| Parser/state-machine fuzz campaign | 10.4 | `fuzz` job → *Fuzz campaign* | `scripts/fuzz_campaign_e2e.sh` (`cargo fuzz`) | `fuzz_campaign_e2e.jsonl`, minimized repros under `fuzz/artifacts/` |

All gate artifacts are uploaded by CI:
- `frankenterm-conformance-gate-artifacts` (conformance + differential + ws)
- `fuzz-campaign-artifacts` (fuzz JSONL + crash repros)

## Running locally

```bash
# Conformance (rch-optional: uses the build fleet locally, bare cargo in CI)
./scripts/vt_support_matrix_e2e.sh /tmp/frankenterm_gates/vt

# Differential vs xterm.js shared fixtures
./tests/e2e/scripts/test_xterm_shared_fixture_differential.sh

# WebSocket protocol compliance
./tests/e2e/scripts/test_ws_protocol_compliance.sh

# Adversarial security/reliability (flow-control + link + clipboard policy)
./scripts/frankenterm_js_security_reliability_compat.sh

# Fuzz campaign (short budget; raise FUZZ_DURATION_SECS for soak)
FUZZ_DURATION_SECS=20 ./scripts/fuzz_campaign_e2e.sh
```

Determinism: pass `E2E_DETERMINISTIC=1 E2E_SEED=0` for stable hashes/logs.

## Triage by gate

### Conformance failed
A declared capability in the VT support matrix was not satisfied. Inspect the
summary and the per-capability cells:

```bash
jq '.' /tmp/frankenterm_gates/vt/meta/vt_support_matrix_summary.json
jq 'select(.passed==false)' /tmp/frankenterm_gates/vt/meta/vt_support_matrix_results.jsonl
```

The runner tracks declared support-matrix claims directly, so a failure means
either the implementation regressed or the declared claim is wrong. Fix the
behaviour, or update the declared claim if the divergence is intentional.

### Differential failed
A shared fixture produced output that did not match the xterm.js-compatible
baseline and could not be classified as a normalised/expected difference.

```bash
jq 'select(.status=="fail" or .classification=="unexpected")' \
  /tmp/ftui_e2e_logs/xterm_shared_fixture_differential/*.jsonl
```

Expected differences are normalised by the harness; an **unclassified** mismatch
is the actionable signal. Either fix the divergence or, if it is a deliberate,
documented difference, extend the normalisation/classification rules.

### WebSocket compliance failed
A protocol frame violated the compliance contract. Inspect the per-frame JSONL +
report for the offending frame and the expected-vs-actual envelope. Confirm the
`frankenterm_ws_bridge` binary built (the gate builds it first).

### Adversarial security/reliability failed
A hostile/degraded scenario produced an unexpected policy decision. The harness
folds three in-tree arms into one manifest:

- **flow-control** (`ftui-pty`): drop policy, queue caps, overload transitions,
  frame cap, deterministic replay — drives
  `frankenterm_core::flow_control::FlowControlPolicy`, the decision core
  `ftui_pty::ws_bridge` wraps for every websocket-attached PTY.
- **link policy** (`ftui-render`): OSC-8 escape-breakout sanitization.
- **clipboard policy** (`ftui-extras`, feature `clipboard`): OSC-52
  bounded-payload cap.

Inspect the failed cells and the roll-up:

```bash
jq 'select(.passed==false)' \
  /tmp/frankenterm_gates/security_reliability/security_reliability_compat_manifest.jsonl
jq '.' /tmp/frankenterm_gates/security_reliability/security_reliability_compat_summary.json
```

Each cell carries `subsystem`, `scenario`, `case`, `correlation_id`, the policy
`decision ledger` (chosen action / reason / fairness / pause), and a
`failure_injection` flag. A failed `drop_policy` cell is the most severe signal:
it means interactive input could be dropped — fix the policy before release. A
failed `frame_cap` or config-bounds cell usually means the external
`frankenterm-core` registry version changed a documented default; reconcile the
assertion with the intended bound. The live `ws_bridge` PTY/socket path is
covered separately by the WebSocket protocol compliance gate above.

### Fuzz campaign failed
A fuzz target found a crash. The minimized repro is written under
`fuzz/artifacts/<target>/`. Reproduce deterministically and promote it to a
regression test:

```bash
cargo fuzz run <target> fuzz/artifacts/<target>/<crash-input>
```

## Failure-injection coverage

Each suite includes a deliberate failure-injection path so the gate's red signal
is proven actionable, not silent:
- the differential harness classifies an intentionally divergent fixture,
- the fuzz campaign validates that a seeded crash produces a minimized repro
  artifact and a non-zero exit,
- the conformance/ws runners emit a structured rejection envelope (not a panic)
  for an unsatisfied claim / malformed frame,
- the adversarial security/reliability harness drives an interactive-starvation
  bypass, an OSC-8 title-rewrite breakout, and an oversized clipboard exfil; the
  aggregator additionally fails the gate if **no** `failure_injection` cell is
  present, so the adversarial coverage can never silently disappear.

## Notes

- The progress/OSC-9;4 signal and the browser event-emitter parity are validated
  via the out-of-tree `frankenterm-web` package E2E, referenced in the addon and
  advanced-API compatibility manifests (bd-2vr05.14.6 / bd-2vr05.13.6).
- CI gates here are **release-blocking by design**. Do not add `continue-on-error`.
