#!/bin/bash
# FrankenTermJS Adversarial Security/Reliability Compatibility Harness (bd-2vr05.11.6)
#
# Single release-blocking entry point that exercises every in-tree security and
# reliability subsystem under hostile/degraded conditions, captures each one's
# structured incident-grade evidence, and folds it into one unified
# compatibility manifest (JSONL) with per-cell sequence IDs, subsystem tags,
# correlation IDs, a policy decision ledger, and a pass/fail roll-up suitable
# for the xterm.js parity scorecard and operational post-mortems.
#
# Subsystem -> in-tree source (durable):
#   drop_policy / queue_caps / overload / frame_cap / replay
#       -> ftui-pty   frankenterm_js_security_reliability_compat
#          (drives frankenterm_core::flow_control::FlowControlPolicy, the decision
#           core ftui_pty::ws_bridge wraps for every websocket-attached PTY)
#   link_policy
#       -> ftui-render frankenterm_js_security_reliability_compat
#          (OSC-8 hyperlink emitter escape-breakout sanitization)
#   clipboard_policy
#       -> ftui-extras frankenterm_js_security_reliability_compat  (feature: clipboard)
#          (OSC-52 host-managed clipboard bounded-payload policy)
#
# Each arm includes a deliberate failure-injection cell (interactive-starvation
# bypass, OSC-8 title-rewrite breakout, oversized clipboard exfil) so the gate's
# red signal is proven actionable, not silent.
#
# Usage:
#   ./scripts/frankenterm_js_security_reliability_compat.sh [--verbose] [--quick]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

VERBOSE=false
QUICK=false
for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick)      QUICK=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo "  --verbose  Show full cargo output"
            echo "  --quick    Skip the standalone build warmup"
            exit 0
            ;;
    esac
done

LOG_DIR="${LOG_DIR:-/tmp/frankenterm_gates/security_reliability}"
MANIFEST="$LOG_DIR/security_reliability_compat_manifest.jsonl"
SUMMARY="$LOG_DIR/security_reliability_compat_summary.json"
mkdir -p "$LOG_DIR"
: > "$MANIFEST"

PASSED=0
FAILED=0
PREFIX="FTUI_SECURITY_RELIABILITY_COMPAT"

echo "=========================================="
echo " FrankenTermJS Adversarial Security/Reliability Harness (bd-2vr05.11.6)"
echo "=========================================="
echo "  Log directory: $LOG_DIR"
echo ""

# run_target <label> -- <cargo test args...>
# Runs the test target, extracts its FTUI_SECURITY_RELIABILITY_COMPAT cells, and
# appends normalised manifest rows (with a global manifest_seq) to $MANIFEST. The
# subsystem tag comes from each cell's own JSON.
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

if ! $QUICK; then
    echo "  Warming up build (clipboard feature)..."
    ( cd "$PROJECT_ROOT" && cargo build --tests -p ftui-extras --features clipboard >/dev/null 2>&1 ) || true
fi

# --- Subsystem runs -------------------------------------------------------
run_target ftui-pty-flow-control -- \
    cargo test -p ftui-pty --test frankenterm_js_security_reliability_compat || true
run_target ftui-render-link-policy -- \
    cargo test -p ftui-render --test frankenterm_js_security_reliability_compat || true
run_target ftui-extras-clipboard -- \
    cargo test -p ftui-extras --features clipboard --test frankenterm_js_security_reliability_compat || true

# --- Manifest validation + coverage gate ----------------------------------
echo ""
echo "  Validating compatibility manifest..."
MANIFEST="$MANIFEST" SUMMARY="$SUMMARY" python3 - <<'PY'
import json, os, sys
manifest = os.environ["MANIFEST"]
summary_path = os.environ["SUMMARY"]
REQUIRED = {
    "drop_policy",
    "queue_caps",
    "overload",
    "frame_cap",
    "replay",
    "link_policy",
    "clipboard_policy",
}

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
    "event": "security_reliability_compat_summary",
    "total_cells": total,
    "per_subsystem": per,
    "required_subsystems": sorted(REQUIRED),
    "missing_subsystems": sorted(missing),
    "failed_cells": failures,
    "failure_injection_cells": fault_cells,
    "bad_json": bad_json,
    "notes": "Drives the in-tree flow-control policy, OSC-8 link sanitizer, and "
             "OSC-52 clipboard cap. The live ws_bridge PTY/socket path is covered "
             "by tests/e2e/scripts/test_ws_protocol_compliance.sh.",
}
with open(summary_path, "w") as out:
    out.write(json.dumps(summary, indent=2, sort_keys=True) + "\n")

print(f"  Total cells: {total}")
for s in sorted(per):
    print(f"    {s:18s} {per[s]}")
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
