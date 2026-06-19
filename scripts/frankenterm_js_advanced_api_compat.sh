#!/bin/bash
# FrankenTermJS Advanced Host API Compatibility Harness (bd-2vr05.13.6)
#
# Single release-blocking entry point that exercises every in-tree advanced
# host API subsystem (parser hooks, markers/viewport anchors, runtime options,
# and event/IME ordering), captures each subsystem's structured evidence, and
# folds it into one unified compatibility manifest (JSONL) with per-cell
# sequence IDs, subsystem tags, correlation IDs, and a pass/fail roll-up
# suitable for the xterm.js parity scorecard.
#
# Subsystem -> in-tree source (durable; the frankenterm-web WASM package is a
# transient/extracted crate, so its browser-only event-emitter parity E2E lives
# out-of-tree and is referenced in the manifest, not run here):
#   parser_hooks   -> ftui-extras  frankenterm_js_parser_hooks_compat   (FTUI_ADVANCED_API_COMPAT)
#   markers        -> ftui-web     frankenterm_js_markers_compat        (FTUI_ADVANCED_API_COMPAT)
#   runtime_options-> ftui-web     frankenterm_js_runtime_options_e2e   (FTUI_RUNTIME_OPTIONS_MATRIX)
#   events_ime     -> ftui-web     frankenterm_js_a11y_e2e              (FTUI_A11Y_MATRIX)
#
# Usage:
#   ./scripts/frankenterm_js_advanced_api_compat.sh [--verbose] [--quick]

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
            echo "  --quick    Skip the standalone clippy/check gate"
            exit 0
            ;;
    esac
done

TIMESTAMP="$(date +%Y%m%d_%H%M%S 2>/dev/null || echo det)"
LOG_DIR="${LOG_DIR:-/tmp/ftui-frankenterm-js-advanced-api-${TIMESTAMP}}"
MANIFEST="$LOG_DIR/advanced_api_compat_manifest.jsonl"
SUMMARY="$LOG_DIR/advanced_api_compat_summary.json"
mkdir -p "$LOG_DIR"
: > "$MANIFEST"

PASSED=0
FAILED=0

echo "=========================================="
echo " FrankenTermJS Advanced API Compatibility Harness (bd-2vr05.13.6)"
echo "=========================================="
echo "  Log directory: $LOG_DIR"
echo ""

# run_subsystem <subsystem> <evidence-prefix> -- <cargo test args...>
# Runs the test, extracts the evidence lines for the given prefix, and appends
# normalised manifest rows (subsystem-tagged) to $MANIFEST.
run_subsystem() {
    local subsystem="$1"; shift
    local prefix="$1"; shift
    [[ "$1" == "--" ]] && shift
    local raw="$LOG_DIR/${subsystem}.raw.log"
    local cells="$LOG_DIR/${subsystem}.cells.jsonl"

    local rc=0
    ( cd "$PROJECT_ROOT" && "$@" -- --nocapture ) >"$raw" 2>&1 || rc=$?

    # Extract + strip the evidence prefix into per-subsystem JSONL.
    grep "^${prefix} " "$raw" 2>/dev/null | sed "s/^${prefix} //" > "$cells" || true
    local count
    count=$(wc -l < "$cells" 2>/dev/null | tr -d ' ')
    count=${count:-0}

    if [[ "$rc" -ne 0 ]]; then
        FAILED=$((FAILED + 1))
        printf "  %-16s  FAIL  (exit %s)  cells=%s\n" "$subsystem" "$rc" "$count"
        $VERBOSE && tail -20 "$raw"
        return 1
    fi
    if [[ "$count" -eq 0 ]]; then
        FAILED=$((FAILED + 1))
        printf "  %-16s  FAIL  (no evidence cells emitted)\n" "$subsystem"
        return 1
    fi

    PASSED=$((PASSED + 1))
    printf "  %-16s  PASS  cells=%s\n" "$subsystem" "$count"

    # Normalise: tag each cell with the subsystem and a global sequence id.
    SUBSYSTEM="$subsystem" python3 - "$cells" "$MANIFEST" <<'PY'
import json, os, sys
subsystem = os.environ["SUBSYSTEM"]
cells_path, manifest_path = sys.argv[1], sys.argv[2]
# Continue the global sequence across subsystems.
seq = sum(1 for _ in open(manifest_path)) if os.path.exists(manifest_path) else 0
with open(cells_path) as fh, open(manifest_path, "a") as out:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        obj = json.loads(line)  # raises (and fails the run) on malformed JSON
        obj.setdefault("subsystem", subsystem)
        obj["manifest_seq"] = seq
        seq += 1
        out.write(json.dumps(obj, sort_keys=True) + "\n")
PY
}

if ! $QUICK; then
    echo "  Compiling harness targets..."
    ( cd "$PROJECT_ROOT" && cargo build --tests -p ftui-extras --features terminal >/dev/null 2>&1 ) || true
fi

# --- Subsystem runs -------------------------------------------------------
run_subsystem parser_hooks    FTUI_ADVANCED_API_COMPAT   -- \
    cargo test -p ftui-extras --features terminal --test frankenterm_js_parser_hooks_compat || true
run_subsystem markers         FTUI_ADVANCED_API_COMPAT   -- \
    cargo test -p ftui-web --test frankenterm_js_markers_compat || true
run_subsystem runtime_options FTUI_RUNTIME_OPTIONS_MATRIX -- \
    cargo test -p ftui-web --features input-parser --test frankenterm_js_runtime_options_e2e || true
run_subsystem events_ime      FTUI_A11Y_MATRIX           -- \
    cargo test -p ftui-web --features input-parser --test frankenterm_js_a11y_e2e || true

# --- Manifest validation + coverage gate ----------------------------------
echo ""
echo "  Validating compatibility manifest..."
MANIFEST="$MANIFEST" SUMMARY="$SUMMARY" python3 - <<'PY'
import json, os, sys
manifest = os.environ["MANIFEST"]
summary_path = os.environ["SUMMARY"]
REQUIRED = {"parser_hooks", "markers", "runtime_options", "events_ime"}

per = {}
total = 0
failures = 0
bad_json = 0
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

covered = set(per) & REQUIRED
missing = REQUIRED - covered
summary = {
    "event": "advanced_api_compat_summary",
    "total_cells": total,
    "per_subsystem": per,
    "required_subsystems": sorted(REQUIRED),
    "missing_subsystems": sorted(missing),
    "failed_cells": failures,
    "bad_json": bad_json,
    "events_emitter_browser_parity": "out-of-tree (frankenterm-web package E2E: tests/e2e/scripts/test_frankenterm_event_ordering_contract.sh)",
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
sys.exit(0 if ok else 1)
PY
GATE_RC=$?

echo ""
echo "=========================================="
echo "  Subsystems passed: $PASSED  failed: $FAILED"
echo "  Manifest: $MANIFEST"
echo "  Summary:  $SUMMARY"
echo "=========================================="

# Release-blocking: every subsystem ran, all evidence present, no failed cells.
if [[ "$FAILED" -ne 0 || "$GATE_RC" -ne 0 ]]; then
    echo "  RESULT: FAIL"
    exit 1
fi
echo "  RESULT: PASS"
