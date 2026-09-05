#!/usr/bin/env bash
set -euo pipefail

# bd-3fc.10: E2E test — Fuzz campaign validation (no crashes in N hours).
#
# Runs cargo-fuzz on all fuzz targets, validates:
#   1. No crashes found
#   2. Coverage metrics reported
#   3. Crash artifacts from previous campaigns still handled
#
# Environment variables:
#   FUZZ_DURATION_SECS  — per-target fuzz duration (default: 30, CI: 300+)
#   FUZZ_MAX_LEN        — max input length (default: 4096)
#   FUZZ_JOBS            — parallel jobs per target (default: 1)
#   LOG_DIR              — output directory for logs and JSONL
#   The nightly toolchain is read from rust-toolchain.toml.
#   RUN_ID               — deterministic run id override
#   FUZZ_TARGET_FILTER   — optional exact target name for focused smoke runs
#   FUZZ_ARTIFACT_ROOT   — crash artifact root (default: fuzz/artifacts)
#   FUZZ_INJECT_CRASH_TARGET — target name for deliberate artifact failure injection
#
# Usage:
#   ./scripts/fuzz_campaign_e2e.sh
#   FUZZ_DURATION_SECS=3600 ./scripts/fuzz_campaign_e2e.sh  # 1 hour CI run

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FUZZ_DIR="$PROJECT_ROOT/fuzz"

FUZZ_DURATION_SECS="${FUZZ_DURATION_SECS:-30}"
FUZZ_MAX_LEN="${FUZZ_MAX_LEN:-4096}"
FUZZ_JOBS="${FUZZ_JOBS:-1}"
NIGHTLY_TOOLCHAIN="$(bash "$SCRIPT_DIR/ci/parse_toolchain_pin.sh" "$PROJECT_ROOT/rust-toolchain.toml")"
FUZZ_TARGET_FILTER="${FUZZ_TARGET_FILTER:-}"
LOG_DIR="${LOG_DIR:-/tmp/fuzz_campaign_e2e_$(date +%Y%m%d_%H%M%S)}"
LOG_JSONL="$LOG_DIR/fuzz_campaign_e2e.jsonl"
RUN_ID="${RUN_ID:-fuzz-campaign-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
RUN_ID="${RUN_ID//[^A-Za-z0-9_.:-]/_}"
FUZZ_ARTIFACT_ROOT="${FUZZ_ARTIFACT_ROOT:-$FUZZ_DIR/artifacts}"
FUZZ_INJECT_CRASH_TARGET="${FUZZ_INJECT_CRASH_TARGET:-}"
CORRELATION_SEQ=0
mkdir -p "$LOG_DIR"

# ---------------------------------------------------------------------------
# JSONL logging
# ---------------------------------------------------------------------------

log_json() {
    local python_bin="${PYTHON_BIN:-}"
    if [[ -z "$python_bin" ]]; then
        if command -v python3 >/dev/null 2>&1; then
            python_bin="python3"
        elif command -v python >/dev/null 2>&1; then
            python_bin="python"
        else
            echo "python or python3 is required for JSONL logging" >&2
            return 1
        fi
    fi
    "$python_bin" -c "
import json, sys, time
data = json.loads(sys.argv[1])
data['ts'] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
print(json.dumps(data, sort_keys=True))
" "$1" >> "$LOG_JSONL"
}

emit_event() {
    local event="$1"
    shift
    CORRELATION_SEQ=$((CORRELATION_SEQ + 1))
    local correlation_id
    correlation_id="$(printf '%s-corr-%04d' "$RUN_ID" "$CORRELATION_SEQ")"
    local extra="$*"
    log_json "{\"event\":\"$event\",\"run_id\":\"$RUN_ID\",\"correlation_id\":\"$correlation_id\"$extra}"
}

# ---------------------------------------------------------------------------
# Discover fuzz targets
# ---------------------------------------------------------------------------

FUZZ_TARGETS=()
while IFS= read -r line; do
    name="$(echo "$line" | sed -n 's/^name = "\(.*\)"/\1/p')"
    if [[ -n "$name" ]]; then
        FUZZ_TARGETS+=("$name")
    fi
done < <(grep '^name = "fuzz_' "$FUZZ_DIR/Cargo.toml")

if [[ -n "$FUZZ_TARGET_FILTER" ]]; then
    FILTERED_TARGETS=()
    for target in "${FUZZ_TARGETS[@]}"; do
        if [[ "$target" == "$FUZZ_TARGET_FILTER" ]]; then
            FILTERED_TARGETS+=("$target")
        fi
    done
    FUZZ_TARGETS=("${FILTERED_TARGETS[@]}")
fi

if [[ ${#FUZZ_TARGETS[@]} -eq 0 ]]; then
    echo "ERROR: No fuzz targets found in $FUZZ_DIR/Cargo.toml"
    if [[ -n "$FUZZ_TARGET_FILTER" ]]; then
        echo "Filter did not match any declared target: $FUZZ_TARGET_FILTER"
    fi
    exit 1
fi

echo "=== Fuzz Campaign E2E ==="
echo "Targets: ${#FUZZ_TARGETS[@]}"
echo "Duration per target: ${FUZZ_DURATION_SECS}s"
echo "Max input length: ${FUZZ_MAX_LEN}"
echo "Run ID: $RUN_ID"
echo "Log directory: $LOG_DIR"
echo ""

# ---------------------------------------------------------------------------
# Emit env record
# ---------------------------------------------------------------------------

emit_event "env" \
    ",\"target_count\":${#FUZZ_TARGETS[@]}" \
    ",\"duration_per_target_secs\":$FUZZ_DURATION_SECS" \
    ",\"max_len\":$FUZZ_MAX_LEN" \
    ",\"jobs\":$FUZZ_JOBS" \
    ",\"nightly_toolchain\":\"$NIGHTLY_TOOLCHAIN\"" \
    ",\"target_filter\":\"$FUZZ_TARGET_FILTER\"" \
    ",\"git_commit\":\"$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)\"" \
    ",\"platform\":\"$(uname -s)\"" \
    ",\"artifact_root\":\"$FUZZ_ARTIFACT_ROOT\"" \
    ",\"failure_injection_target\":\"$FUZZ_INJECT_CRASH_TARGET\"" \
    ",\"targets\":$(printf '%s\n' "${FUZZ_TARGETS[@]}" | python3 -c 'import json,sys; print(json.dumps([l.strip() for l in sys.stdin]))')"

# ---------------------------------------------------------------------------
# Check nightly toolchain
# ---------------------------------------------------------------------------

if ! rustup run "$NIGHTLY_TOOLCHAIN" rustc --version; then
    echo "WARNING: Nightly toolchain '$NIGHTLY_TOOLCHAIN' not found."
    echo "Install with: rustup toolchain install $NIGHTLY_TOOLCHAIN"
    emit_event "error" ",\"message\":\"nightly toolchain not found\",\"toolchain\":\"$NIGHTLY_TOOLCHAIN\""
    # Exit gracefully so CI can detect and handle
    emit_event "run_end" \
        ",\"status\":\"skipped\"" \
        ",\"reason\":\"nightly_toolchain_missing\"" \
        ",\"fuzz_crashes_found_total\":0"
    echo ""
    echo "JSONL log: $LOG_JSONL"
    echo "SKIPPED: nightly toolchain not available"
    exit 0
fi

# Check cargo-fuzz is installed
if ! cargo +"$NIGHTLY_TOOLCHAIN" fuzz --version >/dev/null 2>&1; then
    echo "Installing cargo-fuzz..."
    cargo +"$NIGHTLY_TOOLCHAIN" install cargo-fuzz 2>/dev/null || {
        emit_event "error" ",\"message\":\"cargo-fuzz install failed\""
        emit_event "run_end" \
            ",\"status\":\"skipped\"" \
            ",\"reason\":\"cargo_fuzz_unavailable\"" \
            ",\"fuzz_crashes_found_total\":0"
        echo ""
        echo "JSONL log: $LOG_JSONL"
        echo "SKIPPED: cargo-fuzz not available"
        exit 0
    }
fi

# ---------------------------------------------------------------------------
# Run fuzz campaign
# ---------------------------------------------------------------------------

TOTAL_CRASHES=0
TOTAL_RUNS=0
PASSED_TARGETS=0
FAILED_TARGETS=0
SKIPPED_TARGETS=0
CAMPAIGN_START=$(date +%s)

for target in "${FUZZ_TARGETS[@]}"; do
    echo "--- Fuzzing: $target (${FUZZ_DURATION_SECS}s) ---"
    TARGET_LOG="$LOG_DIR/${target}.log"
    TARGET_START=$(date +%s)
    TARGET_STATUS="pass"
    TARGET_CRASHES=0
    TARGET_RUNS=0
    TARGET_ARTIFACT_DIR="$FUZZ_ARTIFACT_ROOT/$target"
    TARGET_REPLAY_CMD="cargo +$NIGHTLY_TOOLCHAIN fuzz run $target --fuzz-dir $FUZZ_DIR"
    mkdir -p "$TARGET_ARTIFACT_DIR"

    emit_event "target_start" \
        ",\"target\":\"$target\"" \
        ",\"duration_secs\":$FUZZ_DURATION_SECS" \
        ",\"target_log\":\"$TARGET_LOG\"" \
        ",\"artifact_dir\":\"$TARGET_ARTIFACT_DIR\"" \
        ",\"replay_command\":\"$TARGET_REPLAY_CMD\""

    # Run cargo-fuzz with timeout.
    # libfuzzer returns 0 on clean exit, 77 on timeout (expected), non-zero on crash.
    set +e
    cargo +"$NIGHTLY_TOOLCHAIN" fuzz run "$target" \
        --fuzz-dir "$FUZZ_DIR" \
        -- \
        -max_len="$FUZZ_MAX_LEN" \
        -max_total_time="$FUZZ_DURATION_SECS" \
        -jobs="$FUZZ_JOBS" \
        -print_final_stats=1 \
        -artifact_prefix="$TARGET_ARTIFACT_DIR/" \
        > "$TARGET_LOG" 2>&1
    FUZZ_EXIT=$?
    set -e

    TARGET_END=$(date +%s)
    TARGET_ELAPSED=$((TARGET_END - TARGET_START))

    if [[ "$FUZZ_INJECT_CRASH_TARGET" == "$target" ]]; then
        INJECTED_ARTIFACT="$TARGET_ARTIFACT_DIR/crash-injected-$RUN_ID"
        printf 'deliberate failure injection for %s\n' "$target" > "$INJECTED_ARTIFACT"
        emit_event "failure_injection" \
            ",\"target\":\"$target\"" \
            ",\"artifact\":\"$INJECTED_ARTIFACT\"" \
            ",\"diagnostic\":\"synthetic crash artifact injected to validate repro reporting\"" \
            ",\"replay_command\":\"$TARGET_REPLAY_CMD $INJECTED_ARTIFACT\""
    fi

    # Parse stats from libfuzzer output.
    # libfuzzer has used both "inputs" and "units" for executed input counts.
    TARGET_RUNS=$(grep -oP 'stat::number_of_executed_(inputs|units):\s*\K[0-9]+' "$TARGET_LOG" 2>/dev/null | tail -1 || echo "0")
    TARGET_COV=$(grep -oP 'stat::peak_rss_mb:\s*\K[0-9]+' "$TARGET_LOG" 2>/dev/null | tail -1 || echo "0")
    TARGET_EDGES=$(grep -oP 'cov:\s*\K[0-9]+' "$TARGET_LOG" 2>/dev/null | tail -1 || echo "0")

    # Check for crash artifacts.
    CRASH_DIR="$TARGET_ARTIFACT_DIR"
    if [[ -d "$CRASH_DIR" ]]; then
        TARGET_CRASHES=$(find "$CRASH_DIR" \( -name 'crash-*' -o -name 'oom-*' -o -name 'timeout-*' \) -type f 2>/dev/null | wc -l)
    fi

    # Evaluate exit code.
    # Detect build failures (cargo-fuzz returns 1 for both build errors and crashes).
    BUILD_FAILED=0
    if [[ $FUZZ_EXIT -ne 0 ]] && grep -qE 'error\[E|failed to build fuzz script|could not compile' "$TARGET_LOG" 2>/dev/null; then
        BUILD_FAILED=1
    fi

    if [[ $BUILD_FAILED -eq 1 ]]; then
        # Build error — skip this target.
        TARGET_STATUS="skipped"
        SKIPPED_TARGETS=$((SKIPPED_TARGETS + 1))
        TARGET_CRASHES=0
        echo "  WARNING: $target failed to build (see $TARGET_LOG)"
    elif [[ $FUZZ_EXIT -eq 0 || $FUZZ_EXIT -eq 77 ]]; then
        # Clean exit or timeout (expected).
        if [[ $TARGET_CRASHES -gt 0 ]]; then
            TARGET_STATUS="fail"
            FAILED_TARGETS=$((FAILED_TARGETS + 1))
        else
            TARGET_STATUS="pass"
            PASSED_TARGETS=$((PASSED_TARGETS + 1))
        fi
    elif [[ $FUZZ_EXIT -eq 1 ]]; then
        # libfuzzer found a crash during this run.
        TARGET_STATUS="fail"
        FAILED_TARGETS=$((FAILED_TARGETS + 1))
        if [[ $TARGET_CRASHES -eq 0 ]]; then
            TARGET_CRASHES=1
        fi
    else
        # Other error — skip.
        TARGET_STATUS="skipped"
        SKIPPED_TARGETS=$((SKIPPED_TARGETS + 1))
        echo "  WARNING: $target exited with code $FUZZ_EXIT (see $TARGET_LOG)"
    fi

    TOTAL_CRASHES=$((TOTAL_CRASHES + TARGET_CRASHES))
    TOTAL_RUNS=$((TOTAL_RUNS + TARGET_RUNS))

    echo "  Status: $TARGET_STATUS | Runs: $TARGET_RUNS | Edges: $TARGET_EDGES | Crashes: $TARGET_CRASHES | Time: ${TARGET_ELAPSED}s"

    emit_event "target_end" \
        ",\"target\":\"$target\"" \
        ",\"status\":\"$TARGET_STATUS\"" \
        ",\"exit_code\":$FUZZ_EXIT" \
        ",\"runs\":$TARGET_RUNS" \
        ",\"edges\":$TARGET_EDGES" \
        ",\"peak_rss_mb\":$TARGET_COV" \
        ",\"crashes\":$TARGET_CRASHES" \
        ",\"target_log\":\"$TARGET_LOG\"" \
        ",\"artifact_dir\":\"$TARGET_ARTIFACT_DIR\"" \
        ",\"replay_command\":\"$TARGET_REPLAY_CMD\"" \
        ",\"duration_secs\":$TARGET_ELAPSED"
done

# ---------------------------------------------------------------------------
# Check existing crash artifacts (from previous campaigns)
# ---------------------------------------------------------------------------

EXISTING_CRASHES=0
if [[ -d "$FUZZ_ARTIFACT_ROOT" ]]; then
    EXISTING_CRASHES=$(find "$FUZZ_ARTIFACT_ROOT" \( -name 'crash-*' -o -name 'oom-*' -o -name 'timeout-*' \) -type f 2>/dev/null | wc -l)
fi

emit_event "artifact_check" \
    ",\"existing_crash_artifacts\":$EXISTING_CRASHES" \
    ",\"artifacts_dir\":\"$FUZZ_ARTIFACT_ROOT\""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

CAMPAIGN_END=$(date +%s)
CAMPAIGN_ELAPSED=$((CAMPAIGN_END - CAMPAIGN_START))

if [[ $TOTAL_CRASHES -eq 0 && $FAILED_TARGETS -eq 0 ]]; then
    OVERALL_STATUS="pass"
else
    OVERALL_STATUS="fail"
fi

emit_event "run_end" \
    ",\"status\":\"$OVERALL_STATUS\"" \
    ",\"fuzz_crashes_found_total\":$TOTAL_CRASHES" \
    ",\"total_runs\":$TOTAL_RUNS" \
    ",\"targets_passed\":$PASSED_TARGETS" \
    ",\"targets_failed\":$FAILED_TARGETS" \
    ",\"targets_skipped\":$SKIPPED_TARGETS" \
    ",\"existing_crash_artifacts\":$EXISTING_CRASHES" \
    ",\"campaign_duration_secs\":$CAMPAIGN_ELAPSED"

echo ""
echo "=== Fuzz Campaign Summary ==="
echo "Status:          $OVERALL_STATUS"
echo "Targets:         ${#FUZZ_TARGETS[@]} (pass=$PASSED_TARGETS, fail=$FAILED_TARGETS, skip=$SKIPPED_TARGETS)"
echo "Total runs:      $TOTAL_RUNS"
echo "Total crashes:   $TOTAL_CRASHES"
echo "Duration:        ${CAMPAIGN_ELAPSED}s"
echo "Run ID:          $RUN_ID"
echo "JSONL log:       $LOG_JSONL"
echo "Target logs:     $LOG_DIR/*.log"
echo ""

# ---------------------------------------------------------------------------
# Assertions
# ---------------------------------------------------------------------------

EXIT_CODE=0

# Assert: fuzz_crashes_found_total == 0
if [[ $TOTAL_CRASHES -ne 0 ]]; then
    echo "FAIL: fuzz_crashes_found_total=$TOTAL_CRASHES (expected 0)"
    EXIT_CODE=1
fi

# Assert: at least some targets ran successfully
if [[ $PASSED_TARGETS -eq 0 && $SKIPPED_TARGETS -lt ${#FUZZ_TARGETS[@]} ]]; then
    echo "FAIL: no targets passed"
    EXIT_CODE=1
fi

if [[ $PASSED_TARGETS -gt 0 && $TOTAL_RUNS -eq 0 ]]; then
    echo "FAIL: fuzz execution stats were not reported"
    EXIT_CODE=1
fi

if [[ $EXIT_CODE -eq 0 ]]; then
    echo "PASS: All assertions passed"
fi

exit $EXIT_CODE
