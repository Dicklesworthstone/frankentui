#!/bin/bash
# FrankenTermJS SDK Contract Compatibility Harness (bd-2vr05.9.2)
#
# Single release-blocking entry point that exercises the durable in-tree typed
# SDK event/error model (ftui_web::sdk_event_model) and proves it stays in
# lockstep with the shipped TypeScript definitions
# (crates/ftui-web/sdk/frankenterm-js-events.d.ts). Captures the harness's
# structured evidence and folds it into one compatibility manifest (JSONL) with
# per-cell sequence IDs, subsystem tags, correlation IDs, and a pass/fail
# roll-up for the xterm.js parity scorecard.
#
# Subsystem -> in-tree source (durable; the original apiContract() constants
# shipped inside the transient/out-of-tree frankenterm-web WASM package, so this
# is the in-tree source of truth):
#   event_taxonomy / error_model / buffer_policy / ts_lockstep / determinism
#       -> ftui-web  frankenterm_js_sdk_contract_compat
#          (ftui_web::sdk_event_model + sdk/frankenterm-js-events.d.ts golden)
#
# Usage:
#   ./scripts/frankenterm_js_sdk_contract_compat.sh [--verbose]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

VERBOSE=false
for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose]"
            echo "  --verbose  Show full cargo output"
            exit 0
            ;;
    esac
done

LOG_DIR="${LOG_DIR:-/tmp/frankenterm_gates/sdk_contract}"
MANIFEST="$LOG_DIR/sdk_contract_compat_manifest.jsonl"
SUMMARY="$LOG_DIR/sdk_contract_compat_summary.json"
mkdir -p "$LOG_DIR"
: > "$MANIFEST"

PASSED=0
FAILED=0
PREFIX="FTUI_SDK_CONTRACT_COMPAT"

echo "=========================================="
echo " FrankenTermJS SDK Contract Compatibility Harness (bd-2vr05.9.2)"
echo "=========================================="
echo "  Log directory: $LOG_DIR"
echo ""

# run_target <label> -- <cargo test args...>
run_target() {
    local label="$1"; shift
    [[ "$1" == "--" ]] && shift
    local raw="$LOG_DIR/${label}.raw.log"
    local cells="$LOG_DIR/${label}.cells.jsonl"

    local rc=0
    ( cd "$PROJECT_ROOT" && "$@" -- --nocapture ) >"$raw" 2>&1 || rc=$?

    grep "^${PREFIX} " "$raw" 2>/dev/null | sed "s/^${PREFIX} //" > "$cells" || true
    local count
    count=$(wc -l < "$cells" 2>/dev/null | tr -d ' ')
    count=${count:-0}

    if [[ "$rc" -ne 0 ]]; then
        FAILED=$((FAILED + 1))
        printf "  %-28s  FAIL  (exit %s)  cells=%s\n" "$label" "$rc" "$count"
        $VERBOSE && tail -20 "$raw"
        return 1
    fi
    if [[ "$count" -eq 0 ]]; then
        FAILED=$((FAILED + 1))
        printf "  %-28s  FAIL  (no evidence cells emitted)\n" "$label"
        return 1
    fi

    PASSED=$((PASSED + 1))
    printf "  %-28s  PASS  cells=%s\n" "$label" "$count"

    python3 - "$cells" "$MANIFEST" <<'PY'
import json, os, sys
cells_path, manifest_path = sys.argv[1], sys.argv[2]
seq = sum(1 for _ in open(manifest_path)) if os.path.exists(manifest_path) else 0
with open(cells_path) as fh, open(manifest_path, "a") as out:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        obj = json.loads(line)  # raises (fails the run) on malformed JSON
        obj["manifest_seq"] = seq
        seq += 1
        out.write(json.dumps(obj, sort_keys=True) + "\n")
PY
}

# --- Subsystem run --------------------------------------------------------
run_target ftui-web-sdk-contract -- \
    cargo test -p ftui-web --test frankenterm_js_sdk_contract_compat || true

# --- Manifest validation + coverage gate ----------------------------------
echo ""
echo "  Validating compatibility manifest..."
MANIFEST="$MANIFEST" SUMMARY="$SUMMARY" python3 - <<'PY'
import json, os, sys
manifest = os.environ["MANIFEST"]
summary_path = os.environ["SUMMARY"]
REQUIRED = {"event_taxonomy", "error_model", "buffer_policy", "ts_lockstep", "determinism"}

per = {}
total = failures = bad_json = fault_cells = 0
with open(manifest) as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        total += 1
        try:
            obj = json.loads(line)
        except Exception as e:
            bad_json += 1
            print(f"  BAD JSON: {e}")
            continue
        sub = obj.get("subsystem", "unknown")
        per[sub] = per.get(sub, 0) + 1
        if obj.get("passed") is False:
            failures += 1
        if obj.get("failure_injection") is True:
            fault_cells += 1

missing = REQUIRED - (set(per) & REQUIRED)
summary = {
    "event": "sdk_contract_compat_summary",
    "total_cells": total,
    "per_subsystem": per,
    "required_subsystems": sorted(REQUIRED),
    "missing_subsystems": sorted(missing),
    "failed_cells": failures,
    "failure_injection_cells": fault_cells,
    "bad_json": bad_json,
    "notes": "Drives the in-tree typed SDK event/error model and proves the "
             "committed sdk/frankenterm-js-events.d.ts stays in lockstep. The "
             "original apiContract() constants shipped in the out-of-tree "
             "frankenterm-web WASM package; see docs/spec/frankenterm-web-api.md.",
}
with open(summary_path, "w") as out:
    out.write(json.dumps(summary, indent=2, sort_keys=True) + "\n")

print(f"  Total cells: {total}")
for s in sorted(per):
    print(f"    {s:16s} {per[s]}")
ok = True
if missing:
    print(f"  MISSING required subsystem coverage: {sorted(missing)}"); ok = False
if failures:
    print(f"  FAILED cells: {failures}"); ok = False
if bad_json:
    print(f"  MALFORMED manifest rows: {bad_json}"); ok = False
if fault_cells == 0:
    print("  MISSING failure-injection coverage (need >= 1 fault cell)"); ok = False
sys.exit(0 if ok else 1)
PY
GATE_RC=$?

echo ""
echo "=========================================="
echo "  Targets passed: $PASSED  failed: $FAILED"
echo "  Manifest: $MANIFEST"
echo "  Summary:  $SUMMARY"
echo "=========================================="

if [[ "$FAILED" -ne 0 || "$GATE_RC" -ne 0 ]]; then
    echo "  RESULT: FAIL"
    exit 1
fi
echo "  RESULT: PASS"
