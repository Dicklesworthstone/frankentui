#!/bin/bash
# FrankenTermJS full release rehearsal (bd-2vr05.12.6).
#
# Runs the complete release lane end to end — every compat/conformance arm,
# the SDK validation suite, the stress/soak campaign — and assembles the
# SIGNOFF PACKET: the release-readiness artifacts (parity scorecard, browser
# support matrix, staged rollout plan, go/no-go checklist) next to every
# harvested JSONL evidence stream, with a machine-readable rehearsal summary
# linking evidence to checklist items.
#
# Usage:
#   ./scripts/frankenterm_js_release_rehearsal_e2e.sh [--quick] [--verbose]
#
# The packet lands under $LOG_DIR/signoff_packet/ (override via LOG_DIR).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

# shellcheck source=/dev/null
if [[ -f "$LIB_DIR/common.sh" ]]; then
    source "$LIB_DIR/common.sh"
fi
if [[ -f "$LIB_DIR/logging.sh" ]]; then
    source "$LIB_DIR/logging.sh"
fi
if ! declare -f e2e_timestamp >/dev/null 2>&1; then
    e2e_timestamp() { date -Iseconds; }
fi
if ! declare -f e2e_log_stamp >/dev/null 2>&1; then
    e2e_log_stamp() { date +%Y%m%d_%H%M%S; }
fi
if ! declare -f e2e_now_ms >/dev/null 2>&1; then
    e2e_now_ms() { date +%s%3N; }
fi

VERBOSE=false
QUICK=false
for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick)      QUICK=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            exit 0
            ;;
    esac
done

TIMESTAMP="$(e2e_log_stamp)"
LOG_DIR="${LOG_DIR:-/tmp/ftui-frankenterm-release-rehearsal-${TIMESTAMP}}"
PACKET_DIR="$LOG_DIR/signoff_packet"
EVIDENCE_DIR="$PACKET_DIR/evidence"
mkdir -p "$EVIDENCE_DIR"

PASSED=0
FAILED=0
RESULTS=()

# Run a cargo test target, harvest an evidence prefix into the packet.
# rehearse <label> <prefix|-> <checklist_item> <cargo test args...>
rehearse() {
    local label="$1" prefix="$2" checklist="$3"
    shift 3
    local log="$LOG_DIR/${label}.log"
    local start elapsed exit_code=0
    start=$(e2e_now_ms)
    echo "==> rehearsal: ${label}"
    if $VERBOSE; then
        cargo test "$@" -- --nocapture 2>&1 | tee "$log" || exit_code=$?
    else
        cargo test "$@" -- --nocapture > "$log" 2>&1 || exit_code=$?
    fi
    elapsed=$(( $(e2e_now_ms) - start ))
    local evidence_file="-"
    local evidence_lines=0
    if [[ "$prefix" != "-" ]]; then
        evidence_file="$EVIDENCE_DIR/${label}.jsonl"
        RUN_TS="$(e2e_timestamp)"
        (grep "^${prefix} " "$log" || true) \
            | sed "s/^${prefix} //" \
            | sed "s/^{/{\"ts\":\"$RUN_TS\",/" > "$evidence_file"
        evidence_lines=$(wc -l < "$evidence_file" | tr -d ' ')
    fi
    # Vacuity guard: a lane that ran zero tests (feature-gated out, wrong
    # target, filtered away) must NEVER count as a passing rehearsal lane —
    # silence is not evidence. Sum every libtest result line in the log.
    local tests_ran
    tests_ran=$(awk '/^test result:/ { for (i = 1; i <= NF; i++) if ($(i+1) == "passed;") sum += $i } END { print sum + 0 }' "$log")
    local status="pass"
    if [[ $exit_code -ne 0 ]]; then
        status="fail"
        FAILED=$((FAILED + 1))
    elif [[ "$tests_ran" -eq 0 ]]; then
        status="vacuous"
        FAILED=$((FAILED + 1))
        echo "    VACUOUS: lane ran zero tests (check features/target): $log"
    else
        PASSED=$((PASSED + 1))
    fi
    printf "  %-42s %-8s (%s ms, %s tests, %s evidence lines)\n" "$label" "$status" "$elapsed" "$tests_ran" "$evidence_lines"
    RESULTS+=("{\"lane\":\"$label\",\"status\":\"$status\",\"elapsed_ms\":$elapsed,\"tests_ran\":$tests_ran,\"evidence_lines\":$evidence_lines,\"evidence_file\":\"${evidence_file}\",\"checklist_item\":\"$checklist\"}")
}

echo "============================================================"
echo " FrankenTermJS Release Rehearsal (bd-2vr05.12.6)"
echo " Packet: $PACKET_DIR"
echo "============================================================"

# ── Readiness artifacts (scorecard / matrix / rollout / checklist) ─────────
rehearse "release_readiness_artifacts" "FTUI_RELEASE_READINESS" "parity_scorecard_no_open_blockers" \
    -p ftui-web --lib release_readiness

# Split the four artifacts into named packet files.
python3 - "$EVIDENCE_DIR/release_readiness_artifacts.jsonl" "$PACKET_DIR" <<'PY'
import json, pathlib, sys
src, packet = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
for line in src.read_text().splitlines():
    if not line.strip():
        continue
    doc = json.loads(line)
    name = doc.get("artifact", "unknown")
    (packet / f"{name}.json").write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
PY

# ── Compat + conformance lanes (remote attach, scrollback, selection,
#    search, accessibility, fallback rendering — per the .12.6 scope) ──────
rehearse "sdk_contract" "FTUI_SDK_CONTRACT_COMPAT" "compat_arms_green" \
    -p ftui-web --test frankenterm_js_sdk_contract_compat
rehearse "sdk_adapters_validation" "FTUI_SDK_ADAPTER_COMPAT" "compat_arms_green" \
    -p ftui-web --test frankenterm_js_sdk_validation_e2e
rehearse "markers_selection_search" "FTUI_ADVANCED_API_COMPAT" "compat_arms_green" \
    -p ftui-web --test frankenterm_js_markers_compat
# The security/reliability harness is a 3-arm design (bd-2vr05.11): flow
# control (pty), OSC 8 link policy (render), OSC 52 clipboard policy
# (extras — requires the `clipboard` feature, without which the arm
# compiles to ZERO tests and would be vacuous).
rehearse "security_flow_control" "FTUI_SECURITY_RELIABILITY_COMPAT" "compat_arms_green" \
    -p ftui-pty --test frankenterm_js_security_reliability_compat
rehearse "security_link_policy" "FTUI_SECURITY_RELIABILITY_COMPAT" "compat_arms_green" \
    -p ftui-render --test frankenterm_js_security_reliability_compat
rehearse "security_clipboard_policy" "FTUI_SECURITY_RELIABILITY_COMPAT" "compat_arms_green" \
    -p ftui-extras --features clipboard --test frankenterm_js_security_reliability_compat
rehearse "accessibility_ime" "-" "compat_arms_green" \
    -p ftui-web --features input-parser --test frankenterm_js_a11y_e2e
rehearse "runtime_options_fallback" "FTUI_RUNTIME_OPTIONS_MATRIX" "compat_arms_green" \
    -p ftui-web --features input-parser --test frankenterm_js_runtime_options_e2e

# ── Stress/soak campaign (scrollback churn, input floods, resize storms,
#    rollback-trigger evidence) ────────────────────────────────────────────
if $QUICK; then
    export FTUI_RELEASE_STRESS_ITERS=60
fi
rehearse "stress_soak_campaign" "FTUI_RELEASE_STRESS" "stress_campaign_executed" \
    -p ftui-web --test frankenterm_js_release_stress_e2e

# ── Signoff summary: link every lane to its checklist item ─────────────────
SUMMARY="$PACKET_DIR/rehearsal_summary.json"
{
    echo "{"
    echo "  \"schema\": \"frankenterm-release-rehearsal-v1\","
    echo "  \"generated_at\": \"$(e2e_timestamp)\","
    echo "  \"verdict\": \"$([[ $FAILED -eq 0 ]] && echo GO_FOR_SIGNOFF || echo NO_GO)\","
    echo "  \"lanes_passed\": $PASSED,"
    echo "  \"lanes_failed\": $FAILED,"
    echo "  \"lanes\": ["
    (IFS=,; echo "    ${RESULTS[*]}")
    echo "  ]"
    echo "}"
} > "$SUMMARY"

echo "============================================================"
echo "  Lanes: $((PASSED + FAILED))  Passed: $PASSED  Failed: $FAILED"
echo "  Signoff packet: $PACKET_DIR"
echo "  Summary: $SUMMARY"
echo "============================================================"
python3 -c "import json,sys; json.load(open('$SUMMARY')); print('  summary JSON valid')"

[[ $FAILED -eq 0 ]]
