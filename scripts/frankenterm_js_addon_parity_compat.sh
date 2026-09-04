#!/bin/bash
# FrankenTermJS Addon-Parity Compatibility Harness (bd-2vr05.14.6)
#
# Single release-blocking entry point that exercises every in-tree addon-parity
# subsystem (fit-to-container + web-font metrics, OSC-8 web links, inline-image
# protocol detection, and ligature/shaping fallback), captures each one's
# structured evidence, and folds it into one unified compatibility manifest
# (JSONL) with per-cell sequence IDs, subsystem tags, correlation IDs, and a
# pass/fail roll-up suitable for the xterm.js parity scorecard.
#
# Subsystem -> in-tree source (durable; the progress/OSC-9;4 signal landed only
# in the transient/extracted frankenterm-web package, so its browser parity E2E
# lives out-of-tree and is referenced in the manifest, not run here):
#   fit, links -> ftui-render  frankenterm_js_addon_parity_compat
#   image      -> ftui-extras  frankenterm_js_image_parity_compat   (feature: image)
#   ligature   -> ftui-text    frankenterm_js_ligature_parity_compat
#
# Usage:
#   ./scripts/frankenterm_js_addon_parity_compat.sh [--verbose] [--quick]

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

TIMESTAMP="$(date +%Y%m%d_%H%M%S 2>/dev/null || echo det)"
LOG_DIR="${LOG_DIR:-/tmp/ftui-frankenterm-js-addon-parity-${TIMESTAMP}}"
MANIFEST="$LOG_DIR/addon_parity_compat_manifest.jsonl"
SUMMARY="$LOG_DIR/addon_parity_compat_summary.json"
mkdir -p "$LOG_DIR"
: > "$MANIFEST"

PASSED=0
FAILED=0
PREFIX="FTUI_ADDON_PARITY_COMPAT"

echo "=========================================="
echo " FrankenTermJS Addon-Parity Compatibility Harness (bd-2vr05.14.6)"
echo "=========================================="
echo "  Log directory: $LOG_DIR"
echo ""

# Always surface a failing target's raw log in the CI job output (the panic
# otherwise lives only in a /tmp file CI never uploads). See bd-...6.15.
dump_failure_log() {
    local label="$1" raw="$2"
    echo "::group::${label} raw log"
    grep -nE "panicked at|test result: FAILED|FAILED|error\[" "$raw" 2>/dev/null | head -20 || true
    if $VERBOSE; then
        cat "$raw" 2>/dev/null || true
    else
        tail -80 "$raw" 2>/dev/null || true
    fi
    echo "::endgroup::"
}

# run_target <label> -- <cargo test args...>
# Runs the test target, extracts its FTUI_ADDON_PARITY_COMPAT cells, and appends
# normalised manifest rows (with a global manifest_seq) to $MANIFEST. The
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
        dump_failure_log "$label" "$raw"
        return 1
    fi
    if [[ "$count" -eq 0 ]]; then
        FAILED=$((FAILED + 1))
        printf "  %-28s  FAIL  (no evidence cells emitted)\n" "$label"
        dump_failure_log "$label" "$raw"
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
    echo "  Warming up build (image feature)..."
    ( cd "$PROJECT_ROOT" && cargo build --tests -p ftui-extras --features image >/dev/null 2>&1 ) || true
fi

# --- Subsystem runs -------------------------------------------------------
run_target ftui-render-fit-links -- \
    cargo test -p ftui-render --test frankenterm_js_addon_parity_compat || true
run_target ftui-extras-image -- \
    cargo test -p ftui-extras --features image --test frankenterm_js_image_parity_compat || true
run_target ftui-text-ligature -- \
    cargo test -p ftui-text --test frankenterm_js_ligature_parity_compat || true

# --- Manifest validation + coverage gate ----------------------------------
echo ""
echo "  Validating compatibility manifest..."
MANIFEST="$MANIFEST" SUMMARY="$SUMMARY" python3 - <<'PY'
import json, os, sys
manifest = os.environ["MANIFEST"]
summary_path = os.environ["SUMMARY"]
REQUIRED = {"fit", "links", "image", "ligature"}

per = {}
total = failures = bad_json = 0
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

missing = REQUIRED - (set(per) & REQUIRED)
summary = {
    "event": "addon_parity_compat_summary",
    "total_cells": total,
    "per_subsystem": per,
    "required_subsystems": sorted(REQUIRED),
    "missing_subsystems": sorted(missing),
    "failed_cells": failures,
    "bad_json": bad_json,
    "progress_signal_parity": "out-of-tree (OSC 9;4 lives in the frankenterm-web package; see docs/spec/frankenterm-web-api.md terminal.progress)",
}
with open(summary_path, "w") as out:
    out.write(json.dumps(summary, indent=2, sort_keys=True) + "\n")

print(f"  Total cells: {total}")
for s in sorted(per):
    print(f"    {s:12s} {per[s]}")
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
echo "  Targets passed: $PASSED  failed: $FAILED"
echo "  Manifest: $MANIFEST"
echo "  Summary:  $SUMMARY"
echo "=========================================="

if [[ "$FAILED" -ne 0 || "$GATE_RC" -ne 0 ]]; then
    echo "  RESULT: FAIL"
    exit 1
fi
echo "  RESULT: PASS"
