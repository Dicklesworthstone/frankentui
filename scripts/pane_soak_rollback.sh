#!/usr/bin/env bash
# Pane soak + rollback E2E runner (bd-1pvzq.5)
#
# Exercises the user-visible optimized-pane workflow end to end and proves the
# engine rolls back to the conservative strategy safely under sustained
# interaction, emitting operator-grade JSONL and a self-contained artifact
# bundle for CI upload / postmortem.
#
# What it runs:
#   1. The deterministic soak+rollback driver (cargo test pane_soak_rollback),
#      which drives many rounds of resize interaction, evaluates the assumption
#      monitors each round, and rolls back to conservative on the first
#      violation -- writing per-round JSONL (strategy, fallback, timings, state
#      hashes, mode changes) to $PANE_SOAK_LOG.
#   2. Optionally (--with-smoke) the terminal+web drag/resize smoke suite via
#      scripts/pane_e2e.sh for cross-host interaction coverage.
#
# On success the bundle is at $OUT_DIR. On failure a self-contained failure
# bundle (logs + manifest + repro command) is written under $OUT_DIR/failure.
#
# Usage:
#   ./scripts/pane_soak_rollback.sh
#   ./scripts/pane_soak_rollback.sh --rounds 20 --pressure-round 8
#   ./scripts/pane_soak_rollback.sh --with-smoke
#   ./scripts/pane_soak_rollback.sh --out-dir target/pane-soak/ci

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

OUT_DIR="${PANE_SOAK_OUT_DIR:-${PROJECT_ROOT}/target/pane-soak}"
ROUNDS="${PANE_SOAK_ROUNDS:-12}"
OPS_PER_ROUND="${PANE_SOAK_OPS_PER_ROUND:-16}"
PRESSURE_ROUND="${PANE_SOAK_PRESSURE_ROUND:-6}"
SEED="${PANE_SOAK_SEED:-20644}"
WITH_SMOKE=false

if command -v rch >/dev/null 2>&1; then
    CARGO=(rch exec -- cargo)
    RUNNER="rch"
else
    CARGO=(cargo)
    RUNNER="local"
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --rounds) ROUNDS="$2"; shift 2 ;;
        --ops-per-round) OPS_PER_ROUND="$2"; shift 2 ;;
        --pressure-round) PRESSURE_ROUND="$2"; shift 2 ;;
        --seed) SEED="$2"; shift 2 ;;
        --with-smoke) WITH_SMOKE=true; shift ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

mkdir -p "$OUT_DIR"
SOAK_LOG="${OUT_DIR}/pane_soak_rollback.jsonl"
DRIVER_LOG="${OUT_DIR}/driver.log"
MANIFEST="${OUT_DIR}/manifest.json"
REPRO_CMD="PANE_SOAK_ROUNDS=${ROUNDS} PANE_SOAK_OPS_PER_ROUND=${OPS_PER_ROUND} PANE_SOAK_PRESSURE_ROUND=${PRESSURE_ROUND} PANE_SOAK_SEED=${SEED} PANE_SOAK_LOG=${SOAK_LOG} cargo test -p ftui-layout --test pane_soak_rollback -- --nocapture"

log() { printf '[pane-soak] %s\n' "$1"; }

write_manifest() {
    local status="$1"
    local notes="$2"
    python3 - "$MANIFEST" "$status" "$notes" "$SOAK_LOG" "$DRIVER_LOG" "$RUNNER" \
        "$ROUNDS" "$OPS_PER_ROUND" "$PRESSURE_ROUND" "$SEED" "$REPRO_CMD" <<'PY'
import json, sys, pathlib
(manifest, status, notes, soak_log, driver_log, runner, rounds, ops, pressure, seed, repro) = sys.argv[1:12]
summary = None
log_path = pathlib.Path(soak_log)
if log_path.is_file():
    for line in log_path.read_text().splitlines():
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("event") == "pane_soak_summary":
            summary = obj
pathlib.Path(manifest).write_text(json.dumps({
    "schema": "ftui.pane.soak_rollback_manifest",
    "schema_version": 1,
    "bead": "bd-1pvzq.5",
    "status": status,
    "notes": notes,
    "runner": runner,
    "config": {
        "rounds": int(rounds), "ops_per_round": int(ops),
        "pressure_round": int(pressure), "seed": int(seed),
    },
    "artifacts": {"soak_log": soak_log, "driver_log": driver_log},
    "summary": summary,
    "repro": repro,
}, indent=2, sort_keys=True) + "\n")
PY
}

emit_failure_bundle() {
    local reason="$1"
    local fail_dir="${OUT_DIR}/failure"
    mkdir -p "$fail_dir"
    [[ -f "$SOAK_LOG" ]] && cp "$SOAK_LOG" "$fail_dir/" || true
    [[ -f "$DRIVER_LOG" ]] && cp "$DRIVER_LOG" "$fail_dir/" || true
    {
        echo "reason=${reason}"
        echo "repro=${REPRO_CMD}"
        echo "runner=${RUNNER}"
    } > "$fail_dir/failure.txt"
    write_manifest "fail" "$reason"
    cp "$MANIFEST" "$fail_dir/" 2>/dev/null || true
    log "FAILURE bundle written to ${fail_dir} (reason: ${reason})"
}

# ---------------------------------------------------------------------------
# 1. Run the soak + rollback driver
# ---------------------------------------------------------------------------
log "soak driver: rounds=${ROUNDS} ops/round=${OPS_PER_ROUND} pressure_round=${PRESSURE_ROUND} seed=${SEED} runner=${RUNNER}"
if ! PANE_SOAK_LOG="$SOAK_LOG" \
     PANE_SOAK_ROUNDS="$ROUNDS" \
     PANE_SOAK_OPS_PER_ROUND="$OPS_PER_ROUND" \
     PANE_SOAK_PRESSURE_ROUND="$PRESSURE_ROUND" \
     PANE_SOAK_SEED="$SEED" \
     "${CARGO[@]}" test -p ftui-layout --test pane_soak_rollback -- --nocapture \
         > "$DRIVER_LOG" 2>&1; then
    tail -n 30 "$DRIVER_LOG" || true
    emit_failure_bundle "soak_driver_failed"
    exit 1
fi
log "soak driver passed"

# Reconstruct the JSONL locally from the driver's stdout markers if the file did
# not materialize here (e.g. the driver executed on a remote rch worker whose
# filesystem is not synced back). The driver prints each line as SOAK_JSONL=...
if [[ ! -s "$SOAK_LOG" ]] && grep -q 'SOAK_JSONL=' "$DRIVER_LOG" 2>/dev/null; then
    grep -oE 'SOAK_JSONL=.*$' "$DRIVER_LOG" | sed 's/^SOAK_JSONL=//' > "$SOAK_LOG"
    log "reconstructed soak log from driver stdout -> ${SOAK_LOG}"
fi

# ---------------------------------------------------------------------------
# 2. Validate the operator-grade JSONL contract
# ---------------------------------------------------------------------------
if [[ ! -s "$SOAK_LOG" ]]; then
    emit_failure_bundle "soak_log_missing"
    exit 1
fi

VALIDATION="$(python3 - "$SOAK_LOG" <<'PY'
import json, sys
path = sys.argv[1]
rounds = rollbacks = summary = 0
certified = None
required_round_keys = {"round", "strategy", "reason", "replay_depth", "retention_outcome",
                       "monitor_worst", "violations", "rolled_back", "state_hash"}
for n, line in enumerate(open(path), 1):
    line = line.strip()
    if not line:
        continue
    try:
        obj = json.loads(line)
    except json.JSONDecodeError as exc:
        print(f"FAIL malformed JSON at line {n}: {exc}")
        sys.exit(0)
    ev = obj.get("event")
    if ev == "pane_soak_round":
        missing = required_round_keys - set(obj)
        if missing:
            print(f"FAIL round event missing keys {sorted(missing)} at line {n}")
            sys.exit(0)
        rounds += 1
    elif ev == "pane_soak_rollback":
        rollbacks += 1
    elif ev == "pane_soak_summary":
        summary += 1
        certified = obj.get("certified")
if rounds == 0:
    print("FAIL no round events"); sys.exit(0)
if rollbacks != 1:
    print(f"FAIL expected exactly 1 rollback event, got {rollbacks}"); sys.exit(0)
if summary != 1:
    print(f"FAIL expected exactly 1 summary event, got {summary}"); sys.exit(0)
if certified is not True:
    print(f"FAIL summary not certified (certified={certified})"); sys.exit(0)
print(f"OK rounds={rounds} rollbacks={rollbacks} certified={certified}")
PY
)"
log "jsonl validation: ${VALIDATION}"
if [[ "$VALIDATION" != OK* ]]; then
    emit_failure_bundle "jsonl_validation_failed:${VALIDATION}"
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. Optional: terminal + web drag/resize smoke for cross-host coverage
# ---------------------------------------------------------------------------
if [[ "$WITH_SMOKE" == "true" ]]; then
    log "running terminal+web drag/resize smoke (scripts/pane_e2e.sh --mode smoke)"
    if ! "${PROJECT_ROOT}/scripts/pane_e2e.sh" --mode smoke > "${OUT_DIR}/pane_e2e_smoke.log" 2>&1; then
        tail -n 30 "${OUT_DIR}/pane_e2e_smoke.log" || true
        emit_failure_bundle "pane_e2e_smoke_failed"
        exit 1
    fi
    log "pane_e2e smoke passed"
fi

write_manifest "pass" "soak+rollback certified; ${VALIDATION}"
log "PASS — bundle at ${OUT_DIR}"
log "  soak log:  ${SOAK_LOG}"
log "  manifest:  ${MANIFEST}"
