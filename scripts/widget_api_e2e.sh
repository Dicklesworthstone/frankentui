#!/bin/bash
# Widget API E2E Test Script for FrankenTUI
# bd-34lz: Comprehensive verification of Widget API with detailed logging
#
# This script validates:
# 1. Workspace builds successfully
# 2. All unit tests pass
# 3. Clippy finds no warnings
# 4. All feature combinations compile
# 5. Documentation builds
# 6. Widget signatures use Frame (not Buffer)
# 7. Snapshot tests pass (if available)
#
# Usage:
#   ./scripts/widget_api_e2e.sh              # Run all tests
#   ./scripts/widget_api_e2e.sh --verbose    # Extra output
#   ./scripts/widget_api_e2e.sh --quick      # Skip slow steps
#   LOG_DIR=/path/to/logs ./scripts/widget_api_e2e.sh  # Custom log dir

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
LOG_DIR="${LOG_DIR:-/tmp/widget_api_e2e_${TIMESTAMP}}"
E2E_LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"

VERBOSE=false
QUICK=false
STEP_COUNT=0
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

# Seed for deterministic runs
SEED="${FTUI_HARNESS_SEED:-0}"
export FTUI_HARNESS_SEED="$SEED"

# Optional shared E2E helpers (PTY runner)
if [[ -f "$E2E_LIB_DIR/common.sh" ]]; then
    # shellcheck source=/dev/null
    source "$E2E_LIB_DIR/common.sh"
fi
if [[ -f "$E2E_LIB_DIR/pty.sh" ]]; then
    # shellcheck source=/dev/null
    source "$E2E_LIB_DIR/pty.sh"
fi

# Resolve python for PTY runner if available
if [[ -z "${E2E_PYTHON:-}" ]]; then
    if command -v python3 >/dev/null 2>&1; then
        E2E_PYTHON="$(command -v python3)"
        export E2E_PYTHON
    elif command -v python >/dev/null 2>&1; then
        E2E_PYTHON="$(command -v python)"
        export E2E_PYTHON
    fi
fi

# Parse arguments
for arg in "$@"; do
    case $arg in
        --verbose|-v)
            VERBOSE=true
            ;;
        --quick|-q)
            QUICK=true
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo ""
            echo "Options:"
            echo "  --verbose, -v   Show detailed output during execution"
            echo "  --quick, -q     Skip slow steps (docs, some feature combos)"
            echo "  --help, -h      Show this help message"
            echo ""
            echo "Environment:"
            echo "  LOG_DIR         Directory for log files (default: /tmp/widget_api_e2e_TIMESTAMP)"
            exit 0
            ;;
    esac
done

# ============================================================================
# Logging Functions
# ============================================================================

log_info() {
    echo -e "\033[1;34m[INFO]\033[0m $*"
}

log_pass() {
    echo -e "\033[1;32m[PASS]\033[0m $*"
}

log_fail() {
    echo -e "\033[1;31m[FAIL]\033[0m $*"
}

log_skip() {
    echo -e "\033[1;33m[SKIP]\033[0m $*"
}

log_step() {
    STEP_COUNT=$((STEP_COUNT + 1))
    echo ""
    echo -e "\033[1;36m[$STEP_COUNT/$TOTAL_STEPS]\033[0m $*"
}

# ============================================================================
# Step Runner
# ============================================================================

run_step() {
    local step_name="$1"
    local log_file="$2"
    shift 2
    local cmd=("$@")

    log_step "$step_name"

    local start_time
    start_time=$(date +%s.%N)

    if $VERBOSE; then
        if "${cmd[@]}" 2>&1 | tee "$log_file"; then
            local end_time
            end_time=$(date +%s.%N)
            local duration
            duration=$(echo "$end_time - $start_time" | bc)
            log_pass "$step_name completed in ${duration}s"
            PASS_COUNT=$((PASS_COUNT + 1))
            return 0
        else
            log_fail "$step_name failed. See: $log_file"
            FAIL_COUNT=$((FAIL_COUNT + 1))
            return 1
        fi
    else
        if "${cmd[@]}" > "$log_file" 2>&1; then
            local end_time
            end_time=$(date +%s.%N)
            local duration
            duration=$(echo "$end_time - $start_time" | bc)
            log_pass "$step_name completed in ${duration}s"
            PASS_COUNT=$((PASS_COUNT + 1))
            return 0
        else
            log_fail "$step_name failed. See: $log_file"
            FAIL_COUNT=$((FAIL_COUNT + 1))
            return 1
        fi
    fi
}

skip_step() {
    local step_name="$1"
    log_step "$step_name"
    log_skip "Skipped (--quick mode)"
    SKIP_COUNT=$((SKIP_COUNT + 1))
}

# ============================================================================
# Policy Toggle E2E Helpers
# ============================================================================

bool_json() {
    case "${1:-}" in
        1|true|TRUE|True|yes|YES|on|ON)
            echo "true"
            ;;
        *)
            echo "false"
            ;;
    esac
}

escape_json() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\t/\\t/g; s/\r/\\r/g; s/\n/\\n/g'
}

record_terminal_caps() {
    local output_file="$1"
    {
        echo "Terminal Capabilities"
        echo "====================="
        echo "TERM=${TERM:-}"
        echo "COLORTERM=${COLORTERM:-}"
        echo "NO_COLOR=${NO_COLOR:-}"
        echo ""
        if command -v infocmp >/dev/null 2>&1; then
            echo "infocmp -1:"
            infocmp -1 2>/dev/null || true
        else
            echo "infocmp not available"
        fi
        echo ""
        echo "tput colors: $(tput colors 2>/dev/null || echo N/A)"
        echo "stty -a: $(stty -a 2>/dev/null || echo N/A)"
    } > "$output_file"
}

write_case_meta() {
    local meta_file="$1"
    local case_name="$2"
    local screen_mode="$3"
    local cols="$4"
    local rows="$5"
    local ui_height="$6"
    local diff_bayes="$7"
    local bocpd="$8"
    local conformal="$9"
    local evidence_jsonl="${10}"
    local run_log="${11}"
    local pty_out="${12}"
    local caps_file="${13}"

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg case "$case_name" \
            --arg timestamp "$(date -Iseconds)" \
            --arg seed "$SEED" \
            --arg screen_mode "$screen_mode" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson ui_height "$ui_height" \
            --argjson diff_bayes "$(bool_json "$diff_bayes")" \
            --argjson bocpd "$(bool_json "$bocpd")" \
            --argjson conformal "$(bool_json "$conformal")" \
            --arg evidence_jsonl "$evidence_jsonl" \
            --arg run_log "$run_log" \
            --arg pty_output "$pty_out" \
            --arg caps_file "$caps_file" \
            --arg term "${TERM:-}" \
            --arg colorterm "${COLORTERM:-}" \
            --arg no_color "${NO_COLOR:-}" \
            '{case:$case,timestamp:$timestamp,seed:$seed,screen_mode:$screen_mode,cols:$cols,rows:$rows,ui_height:$ui_height,diff_bayesian:$diff_bayes,bocpd:$bocpd,conformal:$conformal,evidence_jsonl:$evidence_jsonl,run_log:$run_log,pty_output:$pty_output,caps_file:$caps_file,term:$term,colorterm:$colorterm,no_color:$no_color}' \
            > "$meta_file"
    else
        printf '{"case":"%s","timestamp":"%s","seed":"%s","screen_mode":"%s","cols":%s,"rows":%s,"ui_height":%s,"diff_bayesian":%s,"bocpd":%s,"conformal":%s,"evidence_jsonl":"%s","run_log":"%s","pty_output":"%s","caps_file":"%s","term":"%s","colorterm":"%s","no_color":"%s"}\n' \
            "$(escape_json "$case_name")" \
            "$(date -Iseconds)" \
            "$(escape_json "$SEED")" \
            "$(escape_json "$screen_mode")" \
            "$cols" "$rows" "$ui_height" \
            "$(bool_json "$diff_bayes")" \
            "$(bool_json "$bocpd")" \
            "$(bool_json "$conformal")" \
            "$(escape_json "$evidence_jsonl")" \
            "$(escape_json "$run_log")" \
            "$(escape_json "$pty_out")" \
            "$(escape_json "$caps_file")" \
            "$(escape_json "${TERM:-}")" \
            "$(escape_json "${COLORTERM:-}")" \
            "$(escape_json "${NO_COLOR:-}")" \
            > "$meta_file"
    fi
}

run_policy_case() {
    local case_name="$1"
    local screen_mode="$2"
    local cols="$3"
    local rows="$4"
    local ui_height="$5"
    local diff_bayes="$6"
    local bocpd="$7"
    local conformal="$8"
    local policy_dir="$9"
    local harness_bin="${10}"

    local case_dir="$policy_dir/$case_name"
    local evidence_jsonl="$case_dir/evidence.jsonl"
    local run_log="$case_dir/run.log"
    local pty_out="$case_dir/pty_output.pty"
    local caps_file="$case_dir/terminal_caps.txt"
    local meta_file="$case_dir/meta.json"

    mkdir -p "$case_dir"
    record_terminal_caps "$caps_file"
    write_case_meta "$meta_file" "$case_name" "$screen_mode" "$cols" "$rows" "$ui_height" "$diff_bayes" "$bocpd" "$conformal" "$evidence_jsonl" "$run_log" "$pty_out" "$caps_file"

    export FTUI_HARNESS_SCREEN_MODE="$screen_mode"
    export FTUI_HARNESS_UI_HEIGHT="$ui_height"
    export FTUI_HARNESS_VIEW="widget-inspector"
    export FTUI_HARNESS_SUPPRESS_WELCOME=1
    export FTUI_HARNESS_EXIT_AFTER_MS=1200
    export FTUI_HARNESS_DIFF_BAYESIAN="$diff_bayes"
    export FTUI_HARNESS_BOCPD="$bocpd"
    export FTUI_HARNESS_CONFORMAL="$conformal"
    export FTUI_HARNESS_EVIDENCE_JSONL="$evidence_jsonl"

    local start_ms
    start_ms=$(date +%s%3N)
    local exit_code=0

    if [[ -n "${E2E_PYTHON:-}" ]] && type -t pty_run >/dev/null 2>&1; then
        PTY_COLS="$cols" PTY_ROWS="$rows" PTY_TIMEOUT=8 PTY_TEST_NAME="$case_name" \
            pty_run "$pty_out" "$harness_bin" > "$run_log" 2>&1 || exit_code=$?
    else
        if command -v timeout >/dev/null 2>&1; then
            TERM="${TERM:-xterm-256color}" \
                timeout 8 "$harness_bin" > "$run_log" 2>&1 || exit_code=$?
        else
            TERM="${TERM:-xterm-256color}" \
                "$harness_bin" > "$run_log" 2>&1 || exit_code=$?
        fi
    fi

    local end_ms
    end_ms=$(date +%s%3N)
    local duration_ms=$((end_ms - start_ms))

    local status="pass"
    if [[ "$exit_code" -ne 0 ]]; then
        status="fail"
    fi
    if [[ ! -s "$evidence_jsonl" ]]; then
        status="fail"
        exit_code=1
    fi

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg case "$case_name" \
            --arg status "$status" \
            --arg seed "$SEED" \
            --arg screen_mode "$screen_mode" \
            --argjson diff_bayes "$(bool_json "$diff_bayes")" \
            --argjson bocpd "$(bool_json "$bocpd")" \
            --argjson conformal "$(bool_json "$conformal")" \
            --arg evidence_jsonl "$evidence_jsonl" \
            --argjson duration_ms "$duration_ms" \
            '{case:$case,status:$status,seed:$seed,screen_mode:$screen_mode,diff_bayesian:$diff_bayes,bocpd:$bocpd,conformal:$conformal,evidence_jsonl:$evidence_jsonl,duration_ms:$duration_ms}' \
            >> "$policy_dir/policy_runs.jsonl"
    else
        printf '{"case":"%s","status":"%s","seed":"%s","screen_mode":"%s","diff_bayesian":%s,"bocpd":%s,"conformal":%s,"evidence_jsonl":"%s","duration_ms":%s}\n' \
            "$(escape_json "$case_name")" \
            "$status" \
            "$(escape_json "$SEED")" \
            "$(escape_json "$screen_mode")" \
            "$(bool_json "$diff_bayes")" \
            "$(bool_json "$bocpd")" \
            "$(bool_json "$conformal")" \
            "$(escape_json "$evidence_jsonl")" \
            "$duration_ms" \
            >> "$policy_dir/policy_runs.jsonl"
    fi

    return "$exit_code"
}

# ============================================================================
# Main Script
# ============================================================================

TOTAL_STEPS=8

echo "=============================================="
echo "  Widget API E2E Test Suite"
echo "=============================================="
echo ""
echo "Project root: $PROJECT_ROOT"
echo "Log directory: $LOG_DIR"
echo "Started at: $(date -Iseconds)"
# Determine mode string
MODE=""
if $QUICK; then MODE="${MODE}quick "; fi
if $VERBOSE; then MODE="${MODE}verbose "; fi
MODE="${MODE:-normal}"
echo "Mode: ${MODE% }"

mkdir -p "$LOG_DIR"
export E2E_LOG_DIR="$LOG_DIR"
cd "$PROJECT_ROOT"

# Record environment info
{
    echo "Environment Information"
    echo "======================="
    echo "Date: $(date -Iseconds)"
    echo "User: $(whoami)"
    echo "Hostname: $(hostname)"
    echo "Working directory: $(pwd)"
    echo "Rust version: $(rustc --version 2>/dev/null || echo 'N/A')"
    echo "Cargo version: $(cargo --version 2>/dev/null || echo 'N/A')"
    echo ""
    echo "Git status:"
    git status --short 2>/dev/null || echo "Not a git repo"
    echo ""
    echo "Git commit:"
    git log -1 --oneline 2>/dev/null || echo "N/A"
    echo ""
    echo "Harness seed: $SEED"
    echo "E2E_PYTHON: ${E2E_PYTHON:-}"
} > "$LOG_DIR/00_environment.log"

# Step 1: Workspace Build
run_step "Building workspace" "$LOG_DIR/01_build.log" \
    cargo build --workspace

# Step 2: Unit Tests
run_step "Running unit tests" "$LOG_DIR/02_tests.log" \
    cargo test --workspace --lib -- --test-threads=4

# Step 3: Clippy
run_step "Running clippy" "$LOG_DIR/03_clippy.log" \
    cargo clippy --workspace --all-targets -- -D warnings

# Step 4: Feature Combinations
log_step "Testing feature combinations"
{
    echo "Feature combination tests - $(date -Iseconds)"
    echo ""

    # ftui-extras base features
    EXTRAS_FEATURES=("canvas" "charts" "forms" "markdown" "export" "clipboard" "syntax" "image")

    for feature in "${EXTRAS_FEATURES[@]}"; do
        echo "Testing ftui-extras --features $feature ..."
        if cargo check -p ftui-extras --features "$feature" 2>&1; then
            echo "  [PASS] $feature"
        else
            echo "  [FAIL] $feature"
            exit 1
        fi
    done

    echo ""
    echo "=== Visual FX Feature Matrix (bd-l8x9.8.4) ==="
    echo ""

    # Visual FX features - CPU path (required)
    VISUAL_FX_FEATURES=(
        "visual-fx"
        "visual-fx-metaballs"
        "visual-fx-plasma"
        "visual-fx,canvas"
        "visual-fx-metaballs,canvas"
        "visual-fx-plasma,canvas"
    )

    for feature in "${VISUAL_FX_FEATURES[@]}"; do
        echo "Testing ftui-extras --features $feature ..."
        CMD="cargo check -p ftui-extras --features $feature"
        echo "  Command: $CMD"
        if $CMD 2>&1; then
            echo "  [PASS] $feature"
        else
            echo "  [FAIL] $feature"
            echo "  Exit code: $?"
            echo "  Last 200 lines of output:"
            tail -200
            exit 1
        fi
    done

    echo ""
    echo "=== GPU Feature Matrix (optional, may fail without GPU) ==="
    echo ""

    # GPU features - optional, log but don't fail if wgpu not available
    GPU_FEATURES=(
        "fx-gpu,visual-fx"
        "fx-gpu,visual-fx-metaballs"
        "fx-gpu,visual-fx,canvas"
    )

    for feature in "${GPU_FEATURES[@]}"; do
        echo "Testing ftui-extras --features $feature ..."
        CMD="cargo check -p ftui-extras --features $feature"
        echo "  Command: $CMD"
        if $CMD 2>&1; then
            echo "  [PASS] $feature (GPU path compiles)"
        else
            # GPU features may fail on systems without wgpu support
            # Log but don't fail - GPU is strictly optional
            echo "  [WARN] $feature (GPU path not available - this is OK)"
        fi
    done

    echo ""
    echo "Testing ftui-widgets with debug-overlay feature..."
    if cargo check -p ftui-widgets --features debug-overlay 2>&1; then
        echo "  [PASS] debug-overlay"
    else
        echo "  [FAIL] debug-overlay"
        exit 1
    fi

    echo ""
    echo "All feature combinations passed!"

} > "$LOG_DIR/04_features.log" 2>&1 && {
    log_pass "Feature combinations passed"
    PASS_COUNT=$((PASS_COUNT + 1))
} || {
    log_fail "Feature combinations failed. See: $LOG_DIR/04_features.log"
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

# Step 5: Widget Signature Verification
log_step "Verifying Widget signatures"
{
    echo "Widget signature verification - $(date -Iseconds)"
    echo ""

    WIDGET_DIR="$PROJECT_ROOT/crates/ftui-widgets/src"

    echo "Checking for old Widget trait Buffer signatures..."
    # Only match the Widget trait render signature pattern: fn render(&self, area: Rect, buf:
    # Helper methods that take Buffer directly (like render_borders) are expected and allowed.
    OLD_SIGS=$(grep -rn 'fn render(&self, area: Rect, buf: &mut Buffer)' "$WIDGET_DIR"/*.rs 2>/dev/null || true)

    if [ -n "$OLD_SIGS" ]; then
        echo "ERROR: Found old Widget trait Buffer signatures:"
        echo "$OLD_SIGS"
        exit 1
    else
        echo "  No old Widget trait Buffer signatures found"
        echo "  (Helper methods using Buffer directly are allowed)"
    fi

    echo ""
    echo "Checking for new Frame signatures..."
    NEW_SIGS=$(grep -rn 'fn render.*frame: &mut Frame' "$WIDGET_DIR"/*.rs 2>/dev/null || true)

    if [ -z "$NEW_SIGS" ]; then
        echo "WARNING: No Frame signatures found (might be empty or different pattern)"
    else
        echo "Found $(echo "$NEW_SIGS" | wc -l) Frame signatures:"
        echo "$NEW_SIGS"
    fi

    echo ""
    echo "Signature verification passed!"

} > "$LOG_DIR/05_signatures.log" 2>&1 && {
    log_pass "Widget signatures verified (Frame-based API)"
    PASS_COUNT=$((PASS_COUNT + 1))
} || {
    log_fail "Widget signature check failed. See: $LOG_DIR/05_signatures.log"
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

# Step 6: Documentation Build (skip in quick mode)
if $QUICK; then
    skip_step "Building documentation (skipped)"
else
    run_step "Building documentation" "$LOG_DIR/06_docs.log" \
        cargo doc --workspace --no-deps
fi

# Step 7: Snapshot Tests (skip in quick mode)
if $QUICK; then
    skip_step "Running snapshot tests (skipped)"
else
    log_step "Running snapshot tests"
    if [ -f "$PROJECT_ROOT/crates/ftui-harness/tests/widget_snapshots.rs" ]; then
        if cargo test -p ftui-harness --test widget_snapshots > "$LOG_DIR/07_snapshots.log" 2>&1; then
            log_pass "Snapshot tests passed"
            PASS_COUNT=$((PASS_COUNT + 1))
        else
            log_fail "Snapshot tests failed. See: $LOG_DIR/07_snapshots.log"
            FAIL_COUNT=$((FAIL_COUNT + 1))
        fi
    else
        log_skip "Snapshot tests not found"
        SKIP_COUNT=$((SKIP_COUNT + 1))
    fi
fi

# Step 8: Policy Toggle Matrix (diff/BOCPD/conformal)
log_step "Policy toggle matrix (diff/BOCPD/conformal)"
policy_log="$LOG_DIR/08_policy.log"
{
    echo "Policy Toggle Matrix - $(date -Iseconds)"
    echo ""

    policy_dir="$LOG_DIR/policy_runs"
    mkdir -p "$policy_dir"
    : > "$policy_dir/policy_runs.jsonl"

    harness_bin="$PROJECT_ROOT/target/debug/ftui-harness"
    if [[ ! -x "$harness_bin" ]]; then
        echo "ftui-harness binary not found; building..."
        cargo build -p ftui-harness --bin ftui-harness
    fi
    if [[ ! -x "$harness_bin" ]]; then
        echo "ERROR: ftui-harness binary not found at $harness_bin"
        exit 1
    fi

    if [[ -z "${E2E_PYTHON:-}" ]] || ! type -t pty_run >/dev/null 2>&1; then
        echo "PTY runner unavailable; falling back to timeout-based runs."
        echo "E2E_PYTHON=${E2E_PYTHON:-}"
    else
        echo "PTY runner available: $E2E_PYTHON"
    fi

    SCREEN_CASES=(
        "alt 200 60 0"
        "alt 120 40 0"
        "inline 200 60 12"
        "inline 80 24 8"
    )

    total_cases=0
    pass_cases=0
    fail_cases=0

    for screen_case in "${SCREEN_CASES[@]}"; do
        read -r screen_mode cols rows ui_height <<< "$screen_case"
        for diff_bayes in 0 1; do
            for bocpd in 0 1; do
                for conformal in 0 1; do
                    total_cases=$((total_cases + 1))
                    case_name="${screen_mode}_${cols}x${rows}_ui${ui_height}_bayes${diff_bayes}_bocpd${bocpd}_conformal${conformal}"
                    echo "Running policy case: $case_name"
                    if run_policy_case "$case_name" "$screen_mode" "$cols" "$rows" "$ui_height" "$diff_bayes" "$bocpd" "$conformal" "$policy_dir" "$harness_bin"; then
                        echo "  [PASS] $case_name"
                        pass_cases=$((pass_cases + 1))
                    else
                        echo "  [FAIL] $case_name"
                        fail_cases=$((fail_cases + 1))
                    fi
                done
            done
        done
    done

    echo ""
    echo "Policy matrix results: total=$total_cases pass=$pass_cases fail=$fail_cases"

    if [[ "$fail_cases" -ne 0 ]]; then
        exit 1
    fi

} > "$policy_log" 2>&1 && {
    log_pass "Policy toggle matrix completed"
    PASS_COUNT=$((PASS_COUNT + 1))
} || {
    log_fail "Policy toggle matrix failed. See: $policy_log"
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=============================================="
echo "  E2E Test Suite Complete"
echo "=============================================="
echo ""
echo "Ended at: $(date -Iseconds)"
echo "Log directory: $LOG_DIR"
echo ""
echo "Results:"
echo "  Passed: $PASS_COUNT"
echo "  Failed: $FAIL_COUNT"
echo "  Skipped: $SKIP_COUNT"
echo ""

# List log files with sizes
echo "Log files:"
ls -lh "$LOG_DIR"/*.log 2>/dev/null | awk '{print "  " $9 " (" $5 ")"}'

echo ""

# Generate summary file
{
    echo "E2E Test Summary"
    echo "================"
    echo "Date: $(date -Iseconds)"
    echo "Passed: $PASS_COUNT"
    echo "Failed: $FAIL_COUNT"
    echo "Skipped: $SKIP_COUNT"
    echo ""
    echo "Exit code: $( [ $FAIL_COUNT -eq 0 ] && echo 0 || echo 1 )"
} > "$LOG_DIR/SUMMARY.txt"

if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "\033[1;32mAll tests passed!\033[0m"
    exit 0
else
    echo -e "\033[1;31m$FAIL_COUNT test(s) failed!\033[0m"
    exit 1
fi
