#!/bin/bash
# Demo Showcase E2E Test Script for FrankenTUI
# bd-qsbe.22: Comprehensive end-to-end verification of the demo showcase
#
# This script validates:
# 1. Compilation (debug + release)
# 2. Clippy (no warnings)
# 3. Formatting (cargo fmt --check)
# 4. Unit + snapshot tests
# 5. Smoke test (alt-screen with auto-exit)
# 6. Inline mode smoke test
# 7. Screen navigation (cycle all 40 screens)
# 8. Search test (Shakespeare screen)
# 9. Resize test (SIGWINCH handling)
# 10. VisualEffects backdrop test (bd-l8x9.8.2)
# 11. Layout inspector scenarios (bd-iuvb.7)
# 11b. Core navigation + dashboard screens (bd-1av4o.14.1)
# 11c. Editors + markdown + log search (bd-1av4o.14.2)
# 11d. Data viz + tables + charts (bd-1av4o.14.4)
# 11e. VFX + determinism lab + Doom/Quake (bd-1av4o.14.3)
# 11f. Inputs/forms + virtualized search (bd-1av4o.14.5)
# 11g. Terminal caps + inline mode story (bd-1av4o.14.6)
# 12. Terminal capabilities report export (bd-iuvb.6)
# 13. i18n stress lab report export (bd-iuvb.9)
# 14. Widget builder export (bd-iuvb.10)
# 15. Determinism lab report (bd-iuvb.2)
# 16. Hyperlink playground JSONL (bd-iuvb.14)
# 17. Command palette JSONL (bd-iuvb.16)
# 18. Explainability cockpit evidence JSONL (bd-iuvb.4)
#
# Usage:
#   ./scripts/demo_showcase_e2e.sh              # Run all tests
#   ./scripts/demo_showcase_e2e.sh --verbose    # Extra output
#   ./scripts/demo_showcase_e2e.sh --quick      # Compilation + clippy + fmt only
#   LOG_DIR=/path/to/logs ./scripts/demo_showcase_e2e.sh

set -uo pipefail

# ============================================================================
# Configuration
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"
# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
if ! declare -f e2e_timestamp >/dev/null 2>&1; then
    e2e_timestamp() { date -Iseconds; }
fi
if ! declare -f e2e_log_stamp >/dev/null 2>&1; then
    e2e_log_stamp() { date +%Y%m%d_%H%M%S; }
fi
PKG="ftui-demo-showcase"

VERBOSE=false
QUICK=false
STEP_COUNT=0
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

declare -a STEP_NAMES=()
declare -a STEP_STATUSES=()
declare -a STEP_DURATIONS=()

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
            echo "  --quick, -q     Compilation + clippy + fmt only"
            echo "  --help, -h      Show this help message"
            echo ""
            echo "Environment:"
            echo "  LOG_DIR         Directory for log files (default: /tmp/ftui-demo-e2e-TIMESTAMP)"
            exit 0
            ;;
    esac
done

e2e_fixture_init "demo_showcase"
TIMESTAMP="$(e2e_log_stamp)"
RUN_ID="${E2E_RUN_ID:-$TIMESTAMP}"
LOG_DIR="${LOG_DIR:-/tmp/ftui-demo-e2e-${E2E_RUN_ID}-${TIMESTAMP}}"
E2E_LOG_DIR="$LOG_DIR"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$LOG_DIR/demo_showcase_e2e.jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$LOG_DIR"

# ============================================================================
# Logging Functions
# ============================================================================

log_info() {
    echo -e "\033[1;34m[INFO]\033[0m $(date +%H:%M:%S) $*"
}

log_pass() {
    echo -e "\033[1;32m[PASS]\033[0m $(date +%H:%M:%S) $*"
}

log_fail() {
    echo -e "\033[1;31m[FAIL]\033[0m $(date +%H:%M:%S) $*"
}

log_skip() {
    echo -e "\033[1;33m[SKIP]\033[0m $(date +%H:%M:%S) $*"
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
    log_info "Running: ${cmd[*]}"

    local start_ms
    start_ms="$(e2e_now_ms)"
    jsonl_step_start "$step_name"

    local exit_code=0
    if $VERBOSE; then
        if "${cmd[@]}" 2>&1 | tee "$log_file"; then
            exit_code=0
        else
            exit_code=1
        fi
    else
        if "${cmd[@]}" > "$log_file" 2>&1; then
            exit_code=0
        else
            exit_code=1
        fi
    fi

    local duration_ms=$(( $(e2e_now_ms) - start_ms ))
    local duration_s
    duration_s=$(echo "scale=2; $duration_ms / 1000" | bc 2>/dev/null || echo "${duration_ms}ms")

    local stdout_size
    stdout_size=$(wc -c < "$log_file" 2>/dev/null || echo 0)

    STEP_NAMES+=("$step_name")
    STEP_DURATIONS+=("${duration_s}s")

    if [ $exit_code -eq 0 ]; then
        log_pass "$step_name completed in ${duration_s}s (output: ${stdout_size} bytes)"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "$step_name" "success" "$duration_ms"
        return 0
    else
        log_fail "$step_name failed (exit=$exit_code). See: $log_file"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "$step_name" "failed" "$duration_ms"
        return 1
    fi
}

skip_step() {
    local step_name="$1"
    local reason="${2:---quick mode}"
    log_step "$step_name"
    log_skip "Skipped ($reason)"
    SKIP_COUNT=$((SKIP_COUNT + 1))
    STEP_NAMES+=("$step_name")
    STEP_STATUSES+=("SKIP")
    STEP_DURATIONS+=("-")
    jsonl_step_start "$step_name"
    jsonl_step_end "$step_name" "skipped" 0
}

# Run a smoke-test step. Captures exit code and records result.
# Usage: run_smoke_step "step name" "log_file" command...
run_smoke_step() {
    local step_name="$1"
    local log_file="$2"
    shift 2

    log_step "$step_name"
    log_info "Running: $*"
    STEP_NAMES+=("$step_name")

    local start_ms
    start_ms="$(e2e_now_ms)"
    jsonl_step_start "$step_name"

    local exit_code=0
    if eval "$@" > "$log_file" 2>&1; then
        exit_code=0
    else
        exit_code=$?
    fi

    local duration_ms=$(( $(e2e_now_ms) - start_ms ))
    local duration_s
    duration_s=$(echo "scale=2; $duration_ms / 1000" | bc 2>/dev/null || echo "${duration_ms}ms")
    STEP_DURATIONS+=("${duration_s}s")

    # exit code 0 = clean exit, 124 = timeout (acceptable for smoke tests)
    if [ $exit_code -eq 0 ] || [ $exit_code -eq 124 ]; then
        log_pass "$step_name passed (exit=$exit_code) in ${duration_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "$step_name" "success" "$duration_ms"
        return 0
    else
        log_fail "$step_name failed (exit=$exit_code). See: $log_file"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "$step_name" "failed" "$duration_ms"
        return 1
    fi
}

# ============================================================================
# PTY Helper
# ============================================================================

# Check whether the `script` command is available for providing a PTY.
has_pty_support() {
    command -v script >/dev/null 2>&1
}

# Run a command inside a pseudo-terminal via script(1).
# This allows the TUI binary to initialize its terminal I/O even in CI.
# Sets a default terminal size of 80x24 unless the command sets its own stty.
# Usage: run_in_pty "command string"
run_in_pty() {
    local cmd="$1"
    # Only add default stty if the command doesn't already set one
    local setup
    if echo "$cmd" | grep -q 'stty'; then
        setup="$cmd"
    else
        setup="stty rows 24 cols 80 2>/dev/null; $cmd"
    fi
    if [ "$(uname)" = "Linux" ]; then
        script -qec "$setup" /dev/null
    else
        script -q /dev/null bash -c "$setup"
    fi
}

# ============================================================================
# Main Script
# ============================================================================

if $QUICK; then
    TOTAL_STEPS=3
else
    TOTAL_STEPS=24  # Updated: added explainability cockpit evidence step
fi

echo "=============================================="
echo "  FrankenTUI Demo Showcase E2E Test Suite"
echo "=============================================="
echo ""
echo "Project root: $PROJECT_ROOT"
echo "Log directory: $LOG_DIR"
echo "Started at:   $(e2e_timestamp)"
MODE=""
if $QUICK; then MODE="${MODE}quick "; fi
if $VERBOSE; then MODE="${MODE}verbose "; fi
MODE="${MODE:-normal}"
echo "Mode:         ${MODE% }"

mkdir -p "$LOG_DIR"
cd "$PROJECT_ROOT"

# Record environment info
{
    echo "Environment Information"
    echo "======================="
    echo "Date: $(e2e_timestamp)"
    echo "User: $(whoami)"
    echo "Hostname: $(hostname)"
    echo "Working directory: $(pwd)"
    echo "Rust version: $(rustc --version 2>/dev/null || echo 'N/A')"
    echo "Cargo version: $(cargo --version 2>/dev/null || echo 'N/A')"
    echo ""
    echo "Git status:"
    git status --short 2>/dev/null | head -20 || echo "Not a git repo"
    echo ""
    echo "Git commit:"
    git log -1 --oneline 2>/dev/null || echo "N/A"
} > "$LOG_DIR/00_environment.log"
jsonl_assert "artifact_env_log" "pass" "env_log=$LOG_DIR/00_environment.log"

# ────────────────────────────────────────────────────────────────────────────
# Step 1: Compilation (debug + release)
# ────────────────────────────────────────────────────────────────────────────
run_step "Compilation (debug + release)" "$LOG_DIR/01_build.log" \
    bash -c "cargo build -p $PKG && cargo build -p $PKG --release" || true

# Resolve binary path via cargo metadata (handles custom target dirs)
TARGET_DIR=$(cargo metadata --format-version=1 -q 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['target_directory'])" 2>/dev/null \
    || echo "$PROJECT_ROOT/target")
BINARY="$TARGET_DIR/release/$PKG"
BINARY_DBG="$TARGET_DIR/debug/$PKG"

# ────────────────────────────────────────────────────────────────────────────
# Step 2: Clippy
# ────────────────────────────────────────────────────────────────────────────
run_step "Clippy (all targets)" "$LOG_DIR/02_clippy.log" \
    cargo clippy -p "$PKG" --all-targets -- -D warnings || true

# ────────────────────────────────────────────────────────────────────────────
# Step 3: Format Check
# ────────────────────────────────────────────────────────────────────────────
run_step "Format check" "$LOG_DIR/03_fmt.log" \
    cargo fmt -p "$PKG" -- --check || true

if $QUICK; then
    # Quick mode stops here — jump to summary
    :
else

# ────────────────────────────────────────────────────────────────────────────
# Step 4: Unit + Snapshot Tests
# ────────────────────────────────────────────────────────────────────────────
run_step "Unit + snapshot tests" "$LOG_DIR/04_tests.log" \
    cargo test -p "$PKG" -- --test-threads=4 || true

# ────────────────────────────────────────────────────────────────────────────
# Steps 5-9: Smoke / Interactive Tests (require PTY)
# ────────────────────────────────────────────────────────────────────────────

CAN_SMOKE=true
SMOKE_REASON=""

if ! has_pty_support; then
    CAN_SMOKE=false
    SMOKE_REASON="script command not available"
fi

if [ ! -x "$BINARY" ] && [ ! -x "$BINARY_DBG" ]; then
    CAN_SMOKE=false
    SMOKE_REASON="binary not found (build may have failed)"
fi

# Prefer release binary, fall back to debug
if [ -x "$BINARY" ]; then
    DEMO_BIN="$BINARY"
elif [ -x "$BINARY_DBG" ]; then
    DEMO_BIN="$BINARY_DBG"
else
    DEMO_BIN=""
fi

if $CAN_SMOKE; then

    # ────────────────────────────────────────────────────────────────────────
    # Step 5: Alt-screen Smoke Test
    # ────────────────────────────────────────────────────────────────────────
    run_smoke_step "Smoke test (alt-screen)" "$LOG_DIR/05_smoke_alt.log" \
        "run_in_pty 'FTUI_DEMO_EXIT_AFTER_MS=3000 timeout 10 $DEMO_BIN'" || true

    # ────────────────────────────────────────────────────────────────────────
    # Step 6: Inline Smoke Test
    # ────────────────────────────────────────────────────────────────────────
    run_smoke_step "Smoke test (inline)" "$LOG_DIR/06_smoke_inline.log" \
        "run_in_pty 'FTUI_DEMO_EXIT_AFTER_MS=3000 FTUI_DEMO_SCREEN_MODE=inline timeout 10 $DEMO_BIN'" || true

    # ────────────────────────────────────────────────────────────────────────
    # Step 7: Screen Navigation
    #
    # Launch the demo on each screen (--screen=1..40) with a
    # short auto-exit. If any screen panics on startup, this catches it.
    # Updated for 40 screens (bd-iuvb.4 explainability cockpit + prior additions)
    # ────────────────────────────────────────────────────────────────────────
    log_step "Screen navigation (all 40 screens)"
    log_info "Starting demo on each screen to verify no panics..."
    NAV_LOG="$LOG_DIR/07_navigation.log"
    STEP_NAMES+=("Screen navigation (all 40)")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Screen navigation (all 40)"
    nav_start_ms="$(e2e_now_ms)"
    {
        NAV_FAILURES=0
        for screen_num in $(seq 1 40); do
            screen_log="$LOG_DIR/07_screen_${screen_num}.log"
            echo "--- Screen $screen_num ---"
            if run_in_pty "stty rows 24 cols 80 2>/dev/null; FTUI_DEMO_EXIT_AFTER_MS=1500 timeout 8 $DEMO_BIN --screen=$screen_num" > "$screen_log" 2>&1; then
                echo "  Screen $screen_num: OK"
                sc_exit=0
            else
                sc_exit=$?
                # 124 = timeout (acceptable if exit_after_ms didn't fire)
                if [ "$sc_exit" -eq 124 ]; then
                    echo "  Screen $screen_num: OK (timeout)"
                else
                    echo "  Screen $screen_num: FAILED (exit=$sc_exit)"
                    NAV_FAILURES=$((NAV_FAILURES + 1))
                fi
            fi

            if [ "$sc_exit" -eq 124 ] || [ "$sc_exit" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
            fi

            hash=$(sha256sum "$screen_log" | awk '{print $1}')
            seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            mode="${E2E_CONTEXT_MODE:-alt}"
            hash_key="$(e2e_hash_key "$mode" 80 24 "$seed_val")"
            jsonl_assert "screen_sweep_${screen_num}" "$status" \
                "screen_num=${screen_num} mode=${mode} cols=80 rows=24 seed=${seed_val} hash_key=${hash_key} hash=${hash} exit=${sc_exit} outcome=${outcome}"
        done
        echo ""
        echo "Screens with failures: $NAV_FAILURES"
        [ "$NAV_FAILURES" -eq 0 ]
    } > "$NAV_LOG" 2>&1
    nav_exit=$?
    nav_dur_ms=$(( $(e2e_now_ms) - nav_start_ms ))
    nav_dur_s=$(echo "scale=2; $nav_dur_ms / 1000" | bc 2>/dev/null || echo "${nav_dur_ms}ms")
    STEP_DURATIONS+=("${nav_dur_s}s")

    if [ $nav_exit -eq 0 ]; then
        log_pass "Screen navigation passed in ${nav_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Screen navigation (all 40)" "success" "$nav_dur_ms"
    else
        log_fail "Screen navigation failed. See: $NAV_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Screen navigation (all 40)" "failed" "$nav_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 8: Search Test (Shakespeare)
    #
    # Start on the Shakespeare screen and verify it renders without panic.
    # The snapshot tests cover search functionality in detail; this verifies
    # the screen survives initialization and a brief run.
    # ────────────────────────────────────────────────────────────────────────
    run_smoke_step "Search test (Shakespeare)" "$LOG_DIR/08_search.log" \
        "run_in_pty 'FTUI_DEMO_EXIT_AFTER_MS=2000 FTUI_DEMO_SCREEN=3 timeout 8 $DEMO_BIN'" || true

    # ────────────────────────────────────────────────────────────────────────
    # Step 9: Resize Test (SIGWINCH)
    #
    # Start the demo at one size, then at a different size. The PTY
    # creation triggers a resize event internally. If the demo survives
    # both sizes without crashing, resize handling works.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Resize test (multiple terminal sizes)"
    log_info "Running demo at 80x24 and 132x43 to verify resize handling..."
    RESIZE_LOG="$LOG_DIR/09_resize.log"
    STEP_NAMES+=("Resize test (multi-size)")

    jsonl_step_start "Resize test (multi-size)"
    resize_start_ms="$(e2e_now_ms)"
    {
        echo "=== Testing at 80x24 ==="
        run_in_pty "stty rows 24 cols 80 2>/dev/null; FTUI_DEMO_EXIT_AFTER_MS=1500 timeout 8 $DEMO_BIN" 2>&1
        exit1=$?
        echo "  Exit code: $exit1"

        echo "=== Testing at 132x43 ==="
        run_in_pty "stty rows 43 cols 132 2>/dev/null; FTUI_DEMO_EXIT_AFTER_MS=1500 timeout 8 $DEMO_BIN" 2>&1
        exit2=$?
        echo "  Exit code: $exit2"

        echo "=== Testing at 40x10 (tiny) ==="
        run_in_pty "stty rows 10 cols 40 2>/dev/null; FTUI_DEMO_EXIT_AFTER_MS=1500 timeout 8 $DEMO_BIN" 2>&1
        exit3=$?
        echo "  Exit code: $exit3"

        # Check all exits (0 or 124 acceptable)
        all_ok=true
        for ec in $exit1 $exit2 $exit3; do
            if [ "$ec" -ne 0 ] && [ "$ec" -ne 124 ]; then
                all_ok=false
            fi
        done
        $all_ok
    } > "$RESIZE_LOG" 2>&1
    resize_exit=$?
    resize_dur_ms=$(( $(e2e_now_ms) - resize_start_ms ))
    resize_dur_s=$(echo "scale=2; $resize_dur_ms / 1000" | bc 2>/dev/null || echo "${resize_dur_ms}ms")
    STEP_DURATIONS+=("${resize_dur_s}s")

    if [ $resize_exit -eq 0 ]; then
        log_pass "Resize test passed in ${resize_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Resize test (multi-size)" "success" "$resize_dur_ms"
    else
        log_fail "Resize test failed. See: $RESIZE_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Resize test (multi-size)" "failed" "$resize_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 10: VisualEffects Backdrop Test (bd-l8x9.8.2)
    #
    # Targeted test for the VisualEffects screen (screen 16) which exercises
    # backdrop blending, metaballs/plasma effects, and markdown-over-backdrop
    # composition paths. Runs at multiple sizes to verify determinism and
    # no panics under various render conditions.
    # ────────────────────────────────────────────────────────────────────────
    log_step "VisualEffects backdrop test (bd-l8x9.8)"
    log_info "Testing VisualEffects screen at multiple sizes..."
    VFX_LOG="$LOG_DIR/10_visual_effects.log"
    VFX_JSONL="$LOG_DIR/10_visual_effects.jsonl"
    STEP_NAMES+=("VisualEffects backdrop")

    : > "$VFX_JSONL"
    jsonl_assert "artifact_vfx_jsonl" "pass" "vfx_jsonl=$VFX_JSONL"
    jsonl_step_start "VisualEffects backdrop"
    vfx_start_ms="$(e2e_now_ms)"
    {
        echo "=== VisualEffects (Screen 16) Backdrop Blending Tests ==="
        echo "Bead: bd-l8x9.8.2 - Targeted runs for metaballs/plasma/backdrop paths"
        echo ""
        VFX_FAILURES=0

        tmux_present=0
        zellij_present=0
        kitty_present=0
        wt_present=0
        if [ -n "${TMUX:-}" ]; then tmux_present=1; fi
        if [ -n "${ZELLIJ:-}" ]; then zellij_present=1; fi
        if [ -n "${KITTY_WINDOW_ID:-}" ]; then kitty_present=1; fi
        if [ -n "${WT_SESSION:-}" ]; then wt_present=1; fi

        vfx_jsonl() {
            local effect="$1"
            local size="$2"
            local outcome="$3"
            local exit_code="$4"
            local duration_ms="$5"
            local rows="$6"
            local cols="$7"
            local ts
            ts="$(e2e_timestamp)"
            local run_id="${RUN_ID}"
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local cols_json="${cols:-0}"
            local rows_json="${rows:-0}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols_json" "$rows_json" "$seed_val")"
            local payload
            payload="{\"schema_version\":\"$(json_escape "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}")\","
            payload="${payload}\"type\":\"visual_effects_case\","
            payload="${payload}\"timestamp\":\"$(json_escape "$ts")\","
            payload="${payload}\"run_id\":\"$(json_escape "$run_id")\","
            payload="${payload}\"seed\":${seed_val},"
            payload="${payload}\"step\":\"visual_effects_backdrop\","
            payload="${payload}\"effect\":\"$(json_escape "$effect")\","
            payload="${payload}\"size\":\"$(json_escape "$size")\","
            payload="${payload}\"mode\":\"$(json_escape "$mode")\","
            payload="${payload}\"hash_key\":\"$(json_escape "$hash_key")\","
            payload="${payload}\"cols\":${cols_json},"
            payload="${payload}\"rows\":${rows_json},"
            payload="${payload}\"screen\":14,"
            payload="${payload}\"exit_code\":${exit_code},"
            payload="${payload}\"duration_ms\":${duration_ms},"
            payload="${payload}\"outcome\":\"$(json_escape "$outcome")\","
            payload="${payload}\"term\":\"$(json_escape "${TERM:-}")\","
            payload="${payload}\"colorterm\":\"$(json_escape "${COLORTERM:-}")\","
            payload="${payload}\"tmux\":${tmux_present},"
            payload="${payload}\"zellij\":${zellij_present},"
            payload="${payload}\"kitty\":${kitty_present},"
            payload="${payload}\"wt\":${wt_present}}"
            echo "$payload" >> "$VFX_JSONL"
            jsonl_emit "$payload"
        }

        run_vfx_case() {
            local effect="$1"
            local effect_env="$2"
            local rows="$3"
            local cols="$4"
            local size="${cols}x${rows}"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; FTUI_DEMO_EXIT_AFTER_MS=2500 ${effect_env} timeout 10 $DEMO_BIN --screen=16"
            local start_ms dur_ms outcome exit_code

            echo "--- ${effect} (${size}) ---"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" 2>&1; then
                outcome="pass"
                exit_code=0
            else
                exit_code=$?
                if [ "$exit_code" -eq 124 ]; then
                    outcome="timeout"
                else
                    outcome="fail"
                    VFX_FAILURES=$((VFX_FAILURES + 1))
                fi
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))
            vfx_jsonl "$effect" "$size" "$outcome" "$exit_code" "$dur_ms" "$rows" "$cols"
        }

        # Metaballs (default effect) — full size matrix
        jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
        run_vfx_case "metaballs" "" 24 80
        jsonl_set_context "alt" 120 40 "${E2E_SEED:-}" 2>/dev/null || true
        run_vfx_case "metaballs" "" 40 120
        jsonl_set_context "alt" 40 10 "${E2E_SEED:-}" 2>/dev/null || true
        run_vfx_case "metaballs" "" 10 40
        jsonl_set_context "alt" 200 24 "${E2E_SEED:-}" 2>/dev/null || true
        run_vfx_case "metaballs" "" 24 200

        # Plasma — explicit effect override
        jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
        run_vfx_case "plasma" "FTUI_DEMO_VFX_EFFECT=plasma" 24 80
        jsonl_set_context "alt" 120 40 "${E2E_SEED:-}" 2>/dev/null || true
        run_vfx_case "plasma" "FTUI_DEMO_VFX_EFFECT=plasma" 40 120

        echo ""
        echo "VisualEffects tests with failures: $VFX_FAILURES"
        [ "$VFX_FAILURES" -eq 0 ]
    } > "$VFX_LOG" 2>&1
    vfx_exit=$?
    vfx_dur_ms=$(( $(e2e_now_ms) - vfx_start_ms ))
    vfx_dur_s=$(echo "scale=2; $vfx_dur_ms / 1000" | bc 2>/dev/null || echo "${vfx_dur_ms}ms")
    STEP_DURATIONS+=("${vfx_dur_s}s")

    if [ $vfx_exit -eq 0 ]; then
        log_pass "VisualEffects backdrop test passed in ${vfx_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "VisualEffects backdrop" "success" "$vfx_dur_ms"
    else
        log_fail "VisualEffects backdrop test failed. See: $VFX_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "VisualEffects backdrop" "failed" "$vfx_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11: Layout Inspector (bd-iuvb.7)
    #
    # Runs the Layout Inspector screen and cycles scenarios/steps to
    # produce deterministic hashes for evidence logs.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Layout Inspector (screen 22)"
    log_info "Running Layout Inspector scenarios and logging hashes..."
    INSPECT_LOG="$LOG_DIR/11_layout_inspector.log"
    INSPECT_JSONL="$LOG_DIR/11_layout_inspector.jsonl"
    STEP_NAMES+=("Layout Inspector")

    : > "$INSPECT_JSONL"
    jsonl_assert "artifact_layout_inspector_jsonl" "pass" "layout_inspector_jsonl=$INSPECT_JSONL"
    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Layout Inspector"
    inspect_start_ms="$(e2e_now_ms)"
    {
        echo "=== Layout Inspector (Screen 22) ==="
        echo "Bead: bd-iuvb.7"
        echo "JSONL: $INSPECT_JSONL"
        echo ""

        inspect_run() {
            local scenario="$1"
            local step="$2"
            local keys="$3"
            local log_file="$LOG_DIR/11_layout_inspector_${scenario}_${step}.log"
            local cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=22 timeout 8 $DEMO_BIN"
            local start_ms dur_ms outcome exit_code rects_hash

            echo "--- Scenario ${scenario} / Step ${step} (keys='${keys}') ---"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ]; then
                outcome="timeout"
            elif [ "$exit_code" -eq 0 ]; then
                outcome="pass"
            else
                outcome="fail"
            fi

            rects_hash=$(sha256sum "$log_file" | awk '{print $1}')

            local ts
            ts="$(e2e_timestamp)"
            local run_id="${RUN_ID}"
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local cols_json="${E2E_CONTEXT_COLS:-80}"
            local rows_json="${E2E_CONTEXT_ROWS:-24}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols_json" "$rows_json" "$seed_val")"
            local payload
            payload="{\"schema_version\":\"$(json_escape "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}")\","
            payload="${payload}\"type\":\"layout_inspector_case\","
            payload="${payload}\"timestamp\":\"$(json_escape "$ts")\","
            payload="${payload}\"run_id\":\"$(json_escape "$run_id")\","
            payload="${payload}\"seed\":${seed_val},"
            payload="${payload}\"mode\":\"$(json_escape "$mode")\","
            payload="${payload}\"hash_key\":\"$(json_escape "$hash_key")\","
            payload="${payload}\"cols\":${cols_json},"
            payload="${payload}\"rows\":${rows_json},"
            payload="${payload}\"screen\":22,"
            payload="${payload}\"scenario_id\":${scenario},"
            payload="${payload}\"step_idx\":${step},"
            payload="${payload}\"keys\":\"$(json_escape "$keys")\","
            payload="${payload}\"rects_hash\":\"$(json_escape "$rects_hash")\","
            payload="${payload}\"duration_ms\":${dur_ms},"
            payload="${payload}\"exit_code\":${exit_code},"
            payload="${payload}\"outcome\":\"$(json_escape "$outcome")\"}"
            echo "$payload" >> "$INSPECT_JSONL"
            jsonl_emit "$payload"
        }

        inspect_run 0 0 ""
        inspect_run 1 1 "n]"
        inspect_run 2 2 "nn]]"
    } > "$INSPECT_LOG" 2>&1
    inspect_exit=$?
    inspect_dur_ms=$(( $(e2e_now_ms) - inspect_start_ms ))
    inspect_dur_s=$(echo "scale=2; $inspect_dur_ms / 1000" | bc 2>/dev/null || echo "${inspect_dur_ms}ms")
    STEP_DURATIONS+=("${inspect_dur_s}s")

    if [ $inspect_exit -eq 0 ]; then
        log_pass "Layout Inspector passed in ${inspect_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Layout Inspector" "success" "$inspect_dur_ms"
    else
        log_fail "Layout Inspector failed. See: $INSPECT_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Layout Inspector" "failed" "$inspect_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11b: Core Navigation + Dashboard Screens (bd-1av4o.14.1)
    #
    # Exercise core demo screens with deterministic key inputs and record
    # per-case hashes via schema-compliant JSONL assert events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Core navigation + dashboard screens"
    log_info "Running dashboard/core screens with key inputs..."
    CORE_LOG="$LOG_DIR/11b_core_nav.log"
    STEP_NAMES+=("Core navigation + dashboard")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Core navigation + dashboard"
    core_start_ms="$(e2e_now_ms)"
    {
        echo "=== Core Navigation + Dashboard Screens ==="
        echo "Bead: bd-1av4o.14.1"
        echo ""

        core_failures=0

        run_core_case() {
            local label="$1"
            local screen_num="$2"
            local keys="$3"
            local cols="${4:-80}"
            local rows="${5:-24}"
            local log_file="$LOG_DIR/11b_core_${label}.log"
            local keys_display
            keys_display="$(printf '%q' "$keys")"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=${screen_num} timeout 8 $DEMO_BIN"
            local start_ms dur_ms exit_code outcome status hash
            local case_name="core_navigation"
            local action="inject_keys"
            local details="screen=${screen_num} keys=${keys_display} cols=${cols} rows=${rows}"

            echo "--- ${label} (screen ${screen_num}, keys=${keys_display}) ---"
            jsonl_case_step_start "$case_name" "$label" "$action" "$details"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ] || [ "$exit_code" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
                core_failures=$((core_failures + 1))
            fi

            hash=$(sha256sum "$log_file" | awk '{print $1}')
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
            jsonl_case_step_end "$case_name" "$label" "$status" "$dur_ms" "$action" "$details"
            jsonl_assert "core_screen_${label}" "$status" \
                "screen=${label} screen_num=${screen_num} keys=${keys_display} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} duration_ms=${dur_ms} exit=${exit_code} outcome=${outcome}"
        }

        run_core_case "dashboard" 2 "cemg" 80 24
        run_core_case "layout_lab" 6 "2d+" 80 24
        run_core_case "performance_hud" 30 "sm" 80 24
        run_core_case "notifications" 19 "s" 80 24
        run_core_case "nav_cycle" 2 $'\t\033[Z' 80 24

        echo ""
        echo "Core screen failures: $core_failures"
        [ "$core_failures" -eq 0 ]
    } > "$CORE_LOG" 2>&1
    core_exit=$?
    core_dur_ms=$(( $(e2e_now_ms) - core_start_ms ))
    core_dur_s=$(echo "scale=2; $core_dur_ms / 1000" | bc 2>/dev/null || echo "${core_dur_ms}ms")
    STEP_DURATIONS+=("${core_dur_s}s")

    if [ $core_exit -eq 0 ]; then
        log_pass "Core navigation + dashboard screens passed in ${core_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Core navigation + dashboard" "success" "$core_dur_ms"
    else
        log_fail "Core navigation + dashboard screens failed. See: $CORE_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Core navigation + dashboard" "failed" "$core_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11c: Editors + Markdown + Log Search (bd-1av4o.14.2)
    #
    # Exercise text-heavy screens with deterministic key inputs and record
    # per-case hashes via schema-compliant JSONL assert events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Editors + Markdown + Log Search"
    log_info "Running editor/markdown/log search screens with key inputs..."
    EDIT_LOG="$LOG_DIR/11c_editors_markdown.log"
    STEP_NAMES+=("Editors + Markdown + Log Search")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Editors + Markdown + Log Search"
    edit_start_ms="$(e2e_now_ms)"
    {
        echo "=== Editors + Markdown + Log Search ==="
        echo "Bead: bd-1av4o.14.2"
        echo ""

        edit_failures=0

        run_editor_case() {
            local label="$1"
            local screen_num="$2"
            local keys="$3"
            local cols="${4:-80}"
            local rows="${5:-24}"
            local log_file="$LOG_DIR/11c_${label}.log"
            local keys_display
            keys_display="$(printf '%q' "$keys")"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & FTUI_DEMO_EXIT_AFTER_MS=2400 FTUI_DEMO_SCREEN=${screen_num} timeout 10 $DEMO_BIN"
            local start_ms dur_ms exit_code outcome status hash
            local case_name="editors_markdown_logsearch"
            local action="inject_keys"
            local details="screen=${screen_num} keys=${keys_display} cols=${cols} rows=${rows}"

            echo "--- ${label} (screen ${screen_num}, keys=${keys_display}) ---"
            jsonl_case_step_start "$case_name" "$label" "$action" "$details"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ] || [ "$exit_code" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
                edit_failures=$((edit_failures + 1))
            fi

            hash=$(sha256sum "$log_file" | awk '{print $1}')
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
            jsonl_case_step_end "$case_name" "$label" "$status" "$dur_ms" "$action" "$details"
            jsonl_assert "editor_screen_${label}" "$status" \
                "screen=${label} screen_num=${screen_num} keys=${keys_display} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} duration_ms=${dur_ms} exit=${exit_code} outcome=${outcome}"
        }

        run_editor_case "advanced_text_editor" 23 $'\x1b[B\x1b[B' 80 24
        run_editor_case "markdown_rich_text" 15 $'\x1b[B\x1b[B' 80 24
        run_editor_case "log_search" 18 $'/err\rn' 80 24
        run_editor_case "command_palette_lab" 36 "m2" 80 24

        echo ""
        echo "Editor/markdown failures: $edit_failures"
        [ "$edit_failures" -eq 0 ]
    } > "$EDIT_LOG" 2>&1
    edit_exit=$?
    edit_dur_ms=$(( $(e2e_now_ms) - edit_start_ms ))
    edit_dur_s=$(echo "scale=2; $edit_dur_ms / 1000" | bc 2>/dev/null || echo "${edit_dur_ms}ms")
    STEP_DURATIONS+=("${edit_dur_s}s")

    if [ $edit_exit -eq 0 ]; then
        log_pass "Editors + Markdown + Log Search passed in ${edit_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Editors + Markdown + Log Search" "success" "$edit_dur_ms"
    else
        log_fail "Editors + Markdown + Log Search failed. See: $EDIT_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Editors + Markdown + Log Search" "failed" "$edit_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11d: Data Viz + Tables + Charts (bd-1av4o.14.4)
    #
    # Exercise data-heavy screens with deterministic key inputs and record
    # per-case hashes via schema-compliant JSONL assert events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Data viz + tables + charts"
    log_info "Running data viz/table/chart screens with key inputs..."
    DATA_LOG="$LOG_DIR/11d_data_viz_tables.log"
    STEP_NAMES+=("Data viz + tables + charts")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Data viz + tables + charts"
    data_start_ms="$(e2e_now_ms)"
    {
        echo "=== Data Viz + Tables + Charts ==="
        echo "Bead: bd-1av4o.14.4"
        echo ""

        data_failures=0

        run_data_case() {
            local label="$1"
            local screen_num="$2"
            local keys="$3"
            local cols="${4:-80}"
            local rows="${5:-24}"
            local log_file="$LOG_DIR/11d_${label}.log"
            local keys_display
            keys_display="$(printf '%q' "$keys")"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=${screen_num} timeout 8 $DEMO_BIN"
            local start_ms dur_ms exit_code outcome status hash
            local case_name="data_viz_tables"
            local action="inject_keys"
            local details="screen=${screen_num} keys=${keys_display} cols=${cols} rows=${rows}"

            echo "--- ${label} (screen ${screen_num}, keys=${keys_display}) ---"
            jsonl_case_step_start "$case_name" "$label" "$action" "$details"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ] || [ "$exit_code" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
                data_failures=$((data_failures + 1))
            fi

            hash=$(sha256sum "$log_file" | awk '{print $1}')
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
            jsonl_case_step_end "$case_name" "$label" "$status" "$dur_ms" "$action" "$details"
            jsonl_assert "data_screen_${label}" "$status" \
                "screen=${label} screen_num=${screen_num} keys=${keys_display} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} duration_ms=${dur_ms} exit=${exit_code} outcome=${outcome}"
        }

        run_data_case "data_viz" 8 $'\x1b[C\x1b[D' 80 24
        run_data_case "table_theme_gallery" 11 "v" 80 24
        run_data_case "widget_gallery" 5 "j" 80 24

        echo ""
        echo "Data viz/table failures: $data_failures"
        [ "$data_failures" -eq 0 ]
    } > "$DATA_LOG" 2>&1
    data_exit=$?
    data_dur_ms=$(( $(e2e_now_ms) - data_start_ms ))
    data_dur_s=$(echo "scale=2; $data_dur_ms / 1000" | bc 2>/dev/null || echo "${data_dur_ms}ms")
    STEP_DURATIONS+=("${data_dur_s}s")

    if [ $data_exit -eq 0 ]; then
        log_pass "Data viz + tables + charts passed in ${data_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Data viz + tables + charts" "success" "$data_dur_ms"
    else
        log_fail "Data viz + tables + charts failed. See: $DATA_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Data viz + tables + charts" "failed" "$data_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11e: VFX + Determinism Lab + Doom/Quake (bd-1av4o.14.3)
    #
    # Exercise visual effects, Doom/Quake modes, determinism lab, and VOI overlay
    # with deterministic inputs and schema-compliant JSONL assert events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "VFX + determinism lab + Doom/Quake"
    log_info "Running VFX/Doom/Quake/determinism lab/VOI overlay..."
    VFX_SWEEP_LOG="$LOG_DIR/11e_vfx_determinism.log"
    STEP_NAMES+=("VFX + determinism lab + Doom/Quake")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "VFX + determinism lab + Doom/Quake"
    vfx_sweep_start_ms="$(e2e_now_ms)"
    {
        echo "=== VFX + Determinism Lab + Doom/Quake ==="
        echo "Bead: bd-1av4o.14.3"
        echo ""

        vfx_failures=0

        run_vfx_screen_case() {
            local label="$1"
            local screen_num="$2"
            local keys="$3"
            local env_prefix="$4"
            local cols="${5:-80}"
            local rows="${6:-24}"
            local log_file="$LOG_DIR/11e_${label}.log"
            local keys_display
            keys_display="$(printf '%q' "$keys")"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & ${env_prefix} FTUI_DEMO_EXIT_AFTER_MS=2400 FTUI_DEMO_SCREEN=${screen_num} timeout 10 $DEMO_BIN"
            local start_ms dur_ms exit_code outcome status hash
            local case_name="vfx_determinism"
            local action="inject_keys"
            local details="screen=${screen_num} keys=${keys_display} env=${env_prefix} cols=${cols} rows=${rows}"

            echo "--- ${label} (screen ${screen_num}, keys=${keys_display}) ---"
            jsonl_case_step_start "$case_name" "$label" "$action" "$details"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ] || [ "$exit_code" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
                vfx_failures=$((vfx_failures + 1))
            fi

            hash=$(sha256sum "$log_file" | awk '{print $1}')
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
            jsonl_case_step_end "$case_name" "$label" "$status" "$dur_ms" "$action" "$details"
            jsonl_assert "vfx_screen_${label}" "$status" \
                "screen=${label} screen_num=${screen_num} keys=${keys_display} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} duration_ms=${dur_ms} exit=${exit_code} outcome=${outcome}"
        }

        run_vfx_screen_case "visual_effects_default" 16 "" "" 80 24
        run_vfx_screen_case "doom_e1m1" 16 " w " "FTUI_DEMO_VFX_EFFECT=doom-e1m1" 80 24
        run_vfx_screen_case "quake_e1m1" 16 " w " "FTUI_DEMO_VFX_EFFECT=quake-e1m1" 80 24
        run_vfx_screen_case "determinism_lab" 37 "e" "" 80 24
        run_vfx_screen_case "voi_overlay" 32 "" "" 80 24

        echo ""
        echo "VFX/determinism failures: $vfx_failures"
        [ "$vfx_failures" -eq 0 ]
    } > "$VFX_SWEEP_LOG" 2>&1
    vfx_sweep_exit=$?
    vfx_sweep_dur_ms=$(( $(e2e_now_ms) - vfx_sweep_start_ms ))
    vfx_sweep_dur_s=$(echo "scale=2; $vfx_sweep_dur_ms / 1000" | bc 2>/dev/null || echo "${vfx_sweep_dur_ms}ms")
    STEP_DURATIONS+=("${vfx_sweep_dur_s}s")

    if [ $vfx_sweep_exit -eq 0 ]; then
        log_pass "VFX + determinism lab + Doom/Quake passed in ${vfx_sweep_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "VFX + determinism lab + Doom/Quake" "success" "$vfx_sweep_dur_ms"
    else
        log_fail "VFX + determinism lab + Doom/Quake failed. See: $VFX_SWEEP_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "VFX + determinism lab + Doom/Quake" "failed" "$vfx_sweep_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11f: Inputs/Forms + Virtualized Search (bd-1av4o.14.5)
    #
    # Exercise forms and virtualized search with deterministic inputs and
    # schema-compliant JSONL assert events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Inputs/forms + virtualized search"
    log_info "Running forms and virtualized search screens..."
    FORMS_LOG="$LOG_DIR/11f_forms_virtualized.log"
    STEP_NAMES+=("Inputs/forms + virtualized search")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Inputs/forms + virtualized search"
    forms_start_ms="$(e2e_now_ms)"
    {
        echo "=== Inputs/Forms + Virtualized Search ==="
        echo "Bead: bd-1av4o.14.5"
        echo ""

        forms_failures=0

        run_forms_case() {
            local label="$1"
            local screen_num="$2"
            local keys="$3"
            local cols="${4:-80}"
            local rows="${5:-24}"
            local log_file="$LOG_DIR/11f_${label}.log"
            local keys_display
            keys_display="$(printf '%q' "$keys")"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & FTUI_DEMO_EXIT_AFTER_MS=2400 FTUI_DEMO_SCREEN=${screen_num} timeout 10 $DEMO_BIN"
            local start_ms dur_ms exit_code outcome status hash
            local case_name="forms_virtualized"
            local action="inject_keys"
            local details="screen=${screen_num} keys=${keys_display} cols=${cols} rows=${rows}"

            echo "--- ${label} (screen ${screen_num}, keys=${keys_display}) ---"
            jsonl_case_step_start "$case_name" "$label" "$action" "$details"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ] || [ "$exit_code" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
                forms_failures=$((forms_failures + 1))
            fi

            hash=$(sha256sum "$log_file" | awk '{print $1}')
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
            jsonl_case_step_end "$case_name" "$label" "$status" "$dur_ms" "$action" "$details"
            jsonl_assert "forms_screen_${label}" "$status" \
                "screen=${label} screen_num=${screen_num} keys=${keys_display} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} duration_ms=${dur_ms} exit=${exit_code} outcome=${outcome}"
        }

        run_forms_case "forms_input" 7 $'a\tb' 80 24
        run_forms_case "form_validation" 25 "m" 80 24
        run_forms_case "virtualized_search" 26 $'/io\rj' 80 24

        echo ""
        echo "Forms/virtualized failures: $forms_failures"
        [ "$forms_failures" -eq 0 ]
    } > "$FORMS_LOG" 2>&1
    forms_exit=$?
    forms_dur_ms=$(( $(e2e_now_ms) - forms_start_ms ))
    forms_dur_s=$(echo "scale=2; $forms_dur_ms / 1000" | bc 2>/dev/null || echo "${forms_dur_ms}ms")
    STEP_DURATIONS+=("${forms_dur_s}s")

    if [ $forms_exit -eq 0 ]; then
        log_pass "Inputs/forms + virtualized search passed in ${forms_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Inputs/forms + virtualized search" "success" "$forms_dur_ms"
    else
        log_fail "Inputs/forms + virtualized search failed. See: $FORMS_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Inputs/forms + virtualized search" "failed" "$forms_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 11g: Terminal Caps + Inline Mode Story (bd-1av4o.14.6)
    #
    # Exercise terminal capabilities screen and inline mode story with
    # deterministic inputs and schema-compliant JSONL assert events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Terminal caps + inline mode story"
    log_info "Running terminal caps and inline mode story screens..."
    INLINE_LOG="$LOG_DIR/11g_terminal_inline.log"
    STEP_NAMES+=("Terminal caps + inline story")

    jsonl_set_context "alt" 80 24 "${E2E_SEED:-}" 2>/dev/null || true
    jsonl_step_start "Terminal caps + inline story"
    inline_start_ms="$(e2e_now_ms)"
    {
        echo "=== Terminal Caps + Inline Mode Story ==="
        echo "Bead: bd-1av4o.14.6"
        echo ""

        inline_failures=0

        run_terminal_case() {
            local label="$1"
            local screen_num="$2"
            local keys="$3"
            local env_prefix="$4"
            local cols="${5:-80}"
            local rows="${6:-24}"
            local log_file="$LOG_DIR/11g_${label}.log"
            local keys_display
            keys_display="$(printf '%q' "$keys")"
            local cmd="stty rows ${rows} cols ${cols} 2>/dev/null; (sleep 0.6; printf \"$keys\" > /dev/tty) & ${env_prefix} FTUI_DEMO_EXIT_AFTER_MS=2400 FTUI_DEMO_SCREEN=${screen_num} timeout 10 $DEMO_BIN"
            local start_ms dur_ms exit_code outcome status hash
            local case_name="terminal_inline"
            local action="inject_keys"
            local details="screen=${screen_num} keys=${keys_display} env=${env_prefix} cols=${cols} rows=${rows}"

            echo "--- ${label} (screen ${screen_num}, keys=${keys_display}) ---"
            jsonl_case_step_start "$case_name" "$label" "$action" "$details"
            start_ms=$(e2e_now_ms)
            if run_in_pty "$cmd" > "$log_file" 2>&1; then
                exit_code=0
            else
                exit_code=$?
            fi
            dur_ms=$(( $(e2e_now_ms) - start_ms ))

            if [ "$exit_code" -eq 124 ] || [ "$exit_code" -eq 0 ]; then
                outcome="pass"
                status="pass"
            else
                outcome="fail"
                status="fail"
                inline_failures=$((inline_failures + 1))
            fi

            hash=$(sha256sum "$log_file" | awk '{print $1}')
            local seed_val="${E2E_CONTEXT_SEED:-${E2E_SEED:-0}}"
            local mode="${E2E_CONTEXT_MODE:-alt}"
            local hash_key
            hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed_val")"
            jsonl_case_step_end "$case_name" "$label" "$status" "$dur_ms" "$action" "$details"
            jsonl_assert "terminal_screen_${label}" "$status" \
                "screen=${label} screen_num=${screen_num} keys=${keys_display} mode=${mode} cols=${cols} rows=${rows} seed=${seed_val} hash_key=${hash_key} hash=${hash} duration_ms=${dur_ms} exit=${exit_code} outcome=${outcome}"
        }

        run_terminal_case "terminal_caps" 12 "e" "" 80 24
        run_terminal_case "inline_mode_story" 33 "" "FTUI_DEMO_SCREEN_MODE=inline FTUI_DEMO_UI_HEIGHT=12" 80 24

        echo ""
        echo "Terminal/inline failures: $inline_failures"
        [ "$inline_failures" -eq 0 ]
    } > "$INLINE_LOG" 2>&1
    inline_exit=$?
    inline_dur_ms=$(( $(e2e_now_ms) - inline_start_ms ))
    inline_dur_s=$(echo "scale=2; $inline_dur_ms / 1000" | bc 2>/dev/null || echo "${inline_dur_ms}ms")
    STEP_DURATIONS+=("${inline_dur_s}s")

    if [ $inline_exit -eq 0 ]; then
        log_pass "Terminal caps + inline mode story passed in ${inline_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Terminal caps + inline story" "success" "$inline_dur_ms"
    else
        log_fail "Terminal caps + inline mode story failed. See: $INLINE_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Terminal caps + inline story" "failed" "$inline_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 12: Terminal Capabilities Report Export (bd-iuvb.6)
    #
    # Runs the Terminal Capabilities screen and triggers an export via
    # an injected 'e' keypress to produce JSONL output.
    # ────────────────────────────────────────────────────────────────────────
    log_step "Terminal caps report (screen 12)"
    log_info "Running TerminalCapabilities and exporting JSONL report..."
    CAPS_LOG="$LOG_DIR/12_terminal_caps.log"
    CAPS_REPORT="$LOG_DIR/12_terminal_caps_report_${TIMESTAMP}.jsonl"
    CAPS_JSONL="$LOG_DIR/12_terminal_caps_summary.jsonl"
    STEP_NAMES+=("Terminal caps report")

    jsonl_step_start "Terminal caps report"
    caps_start_ms="$(e2e_now_ms)"
    {
        echo "=== Terminal Capabilities (Screen 12) Report Export ==="
        echo "Bead: bd-iuvb.6"
        echo "Report path: $CAPS_REPORT"
        echo ""

        caps_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.6; printf 'e' > /dev/tty) & FTUI_TERMCAPS_REPORT_PATH=\"$CAPS_REPORT\" FTUI_DEMO_EXIT_AFTER_MS=2000 FTUI_DEMO_SCREEN=12 timeout 8 $DEMO_BIN"

        if run_in_pty "$caps_cmd" 2>&1; then
            caps_exit=0
        else
            caps_exit=$?
        fi

        if [ "$caps_exit" -eq 124 ]; then
            caps_outcome="timeout"
        elif [ "$caps_exit" -eq 0 ]; then
            caps_outcome="pass"
        else
            caps_outcome="fail"
        fi

        caps_report_ok=false
        if [ -s "$CAPS_REPORT" ]; then
            caps_report_ok=true
        else
            echo "Report file missing or empty: $CAPS_REPORT"
            caps_outcome="no_report"
        fi

        caps_parse_ok=false
        if $caps_report_ok; then
            if python3 - "$CAPS_REPORT" "$CAPS_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$caps_outcome" "$caps_exit" <<'PY'
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

with open(report_path, "r", encoding="utf-8") as handle:
    lines = [line for line in handle if line.strip()]
if not lines:
    raise SystemExit("Report JSONL is empty")

report = json.loads(lines[-1])
caps = report.get("capabilities", [])
enabled = [row.get("capability") for row in caps if row.get("effective") is True]
disabled = [row.get("capability") for row in caps if row.get("effective") is False]

profile = report.get("simulated_profile") or report.get("detected_profile")
try:
    seed_val = int(seed)
except ValueError:
    seed_val = None
payload = {
    "schema_version": schema_version,
    "type": "terminal_caps_summary",
    "timestamp": timestamp,
    "run_id": run_id,
    "seed": seed_val,
    "profile": profile,
    "enabled_features": enabled,
    "disabled_features": disabled,
    "outcome": outcome,
    "exit_code": int(exit_code),
}

with open(summary_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\n")
PY
            then
                caps_parse_ok=true
            else
                echo "Failed to parse report into summary JSONL"
                caps_outcome="parse_fail"
            fi
        fi

        caps_exit_ok=true
        if [ "$caps_exit" -ne 0 ] && [ "$caps_exit" -ne 124 ]; then
            caps_exit_ok=false
        fi

        caps_success=true
        if ! $caps_exit_ok; then caps_success=false; fi
        if ! $caps_report_ok; then caps_success=false; fi
        if ! $caps_parse_ok; then caps_success=false; fi

        echo "Outcome: $caps_outcome"
        echo "Summary JSONL: $CAPS_JSONL"

        $caps_success
    } > "$CAPS_LOG" 2>&1
    caps_exit=$?
    caps_dur_ms=$(( $(e2e_now_ms) - caps_start_ms ))
    caps_dur_s=$(echo "scale=2; $caps_dur_ms / 1000" | bc 2>/dev/null || echo "${caps_dur_ms}ms")
    STEP_DURATIONS+=("${caps_dur_s}s")

    if [ $caps_exit -eq 0 ]; then
        log_pass "Terminal caps report passed in ${caps_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "Terminal caps report" "success" "$caps_dur_ms"
    else
        log_fail "Terminal caps report failed. See: $CAPS_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "Terminal caps report" "failed" "$caps_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 13: i18n Stress Lab Report Export (bd-iuvb.9)
    #
    # Runs the i18n screen (screen 31), cycles to the Stress Lab panel,
    # and exports a JSONL report via an injected 'e' keypress.
    # ────────────────────────────────────────────────────────────────────────
    log_step "i18n stress report (screen 31)"
    log_info "Running i18n Stress Lab and exporting JSONL report..."
    I18N_LOG="$LOG_DIR/13_i18n_stress.log"
    I18N_REPORT="$LOG_DIR/13_i18n_report_${TIMESTAMP}.jsonl"
    I18N_JSONL="$LOG_DIR/13_i18n_summary.jsonl"
    STEP_NAMES+=("i18n stress report")

    jsonl_step_start "i18n stress report"
    i18n_start_ms="$(e2e_now_ms)"
    {
        echo "=== i18n Stress Lab (Screen 31) Report Export ==="
        echo "Bead: bd-iuvb.9"
        echo "Report path: $I18N_REPORT"
        echo ""

        i18n_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.5; printf '\\t\\t\\t' > /dev/tty; sleep 0.2; printf 'e' > /dev/tty) & FTUI_I18N_REPORT_PATH=\"$I18N_REPORT\" FTUI_I18N_REPORT_WIDTH=32 FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=31 timeout 8 $DEMO_BIN"

        if run_in_pty "$i18n_cmd" 2>&1; then
            i18n_exit=0
        else
            i18n_exit=$?
        fi

        if [ "$i18n_exit" -eq 124 ]; then
            i18n_outcome="timeout"
        elif [ "$i18n_exit" -eq 0 ]; then
            i18n_outcome="pass"
        else
            i18n_outcome="fail"
        fi

        i18n_report_ok=false
        if [ -s "$I18N_REPORT" ]; then
            i18n_report_ok=true
        else
            echo "Report file missing or empty: $I18N_REPORT"
            i18n_outcome="no_report"
        fi

        i18n_parse_ok=false
        if $i18n_report_ok; then
            if python3 - "$I18N_REPORT" "$I18N_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$i18n_outcome" "$i18n_exit" <<'PY'
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

with open(report_path, "r", encoding="utf-8") as handle:
    lines = [line for line in handle if line.strip()]
if not lines:
    raise SystemExit("Report JSONL is empty")

report = json.loads(lines[-1])
try:
    seed_val = int(seed)
except ValueError:
    seed_val = None
payload = {
    "schema_version": schema_version,
    "type": "i18n_stress_summary",
    "timestamp": timestamp,
    "run_id": run_id,
    "seed": seed_val,
    "sample_id": report.get("sample_id"),
    "width_metrics": report.get("width_metrics", {}),
    "truncation_state": report.get("truncation_state", {}),
    "outcome": outcome,
    "exit_code": int(exit_code),
}

with open(summary_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\n")
PY
            then
                i18n_parse_ok=true
            else
                echo "Failed to parse report into summary JSONL"
                i18n_outcome="parse_fail"
            fi
        fi

        i18n_exit_ok=true
        if [ "$i18n_exit" -ne 0 ] && [ "$i18n_exit" -ne 124 ]; then
            i18n_exit_ok=false
        fi

        i18n_success=true
        if ! $i18n_exit_ok; then i18n_success=false; fi
        if ! $i18n_report_ok; then i18n_success=false; fi
        if ! $i18n_parse_ok; then i18n_success=false; fi

        echo "Outcome: $i18n_outcome"
        echo "Summary JSONL: $I18N_JSONL"

        $i18n_success
    } > "$I18N_LOG" 2>&1
    i18n_exit=$?
    i18n_dur_ms=$(( $(e2e_now_ms) - i18n_start_ms ))
    i18n_dur_s=$(echo "scale=2; $i18n_dur_ms / 1000" | bc 2>/dev/null || echo "${i18n_dur_ms}ms")
    STEP_DURATIONS+=("${i18n_dur_s}s")

    if [ $i18n_exit -eq 0 ]; then
        log_pass "i18n stress report passed in ${i18n_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "i18n stress report" "success" "$i18n_dur_ms"
    else
        log_fail "i18n stress report failed. See: $I18N_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "i18n stress report" "failed" "$i18n_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 14: Widget Builder Export (bd-iuvb.10)
    #
    # Runs the widget builder (screen 35) and exports a JSONL snapshot.
    # ────────────────────────────────────────────────────────────────────────
    log_step "widget builder export (screen 35)"
    log_info "Running Widget Builder and exporting JSONL snapshot..."
    WIDGET_LOG="$LOG_DIR/14_widget_builder.log"
    WIDGET_REPORT="$LOG_DIR/14_widget_builder_report_${TIMESTAMP}.jsonl"
    WIDGET_JSONL="$LOG_DIR/14_widget_builder_summary.jsonl"
    STEP_NAMES+=("widget builder export")

    jsonl_step_start "widget builder export"
    widget_start_ms="$(e2e_now_ms)"
    {
        echo "=== Widget Builder (Screen 35) Export ==="
        echo "Bead: bd-iuvb.10"
        echo "Report path: $WIDGET_REPORT"
        echo ""

        widget_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.5; printf 'x' > /dev/tty) & FTUI_WIDGET_BUILDER_EXPORT_PATH=\"$WIDGET_REPORT\" FTUI_WIDGET_BUILDER_RUN_ID=\"$RUN_ID\" FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=35 timeout 8 $DEMO_BIN"

        if run_in_pty "$widget_cmd" 2>&1; then
            widget_exit=0
        else
            widget_exit=$?
        fi

        if [ "$widget_exit" -eq 124 ]; then
            widget_outcome="timeout"
        elif [ "$widget_exit" -eq 0 ]; then
            widget_outcome="pass"
        else
            widget_outcome="fail"
        fi

        widget_report_ok=false
        if [ -s "$WIDGET_REPORT" ]; then
            widget_report_ok=true
        else
            echo "Report file missing or empty: $WIDGET_REPORT"
            widget_outcome="no_report"
        fi

        widget_parse_ok=false
        if $widget_report_ok; then
            if python3 - "$WIDGET_REPORT" "$WIDGET_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$widget_outcome" "$widget_exit" <<'PY'
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

with open(report_path, "r", encoding="utf-8") as handle:
    lines = [line for line in handle if line.strip()]
if not lines:
    raise SystemExit("Report JSONL is empty")

report = json.loads(lines[-1])
try:
    seed_val = int(seed)
except ValueError:
    seed_val = None
payload = {
    "schema_version": schema_version,
    "type": "widget_builder_summary",
    "timestamp": timestamp,
    "run_id": report.get("run_id", run_id),
    "seed": seed_val,
    "preset_id": report.get("preset_id"),
    "widget_count": report.get("widget_count"),
    "props_hash": report.get("props_hash"),
    "outcome": outcome,
    "exit_code": int(exit_code),
}

with open(summary_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\\n")
PY
            then
                widget_parse_ok=true
            else
                echo "Failed to parse report into summary JSONL"
                widget_outcome="parse_fail"
            fi
        fi

        widget_exit_ok=true
        if [ "$widget_exit" -ne 0 ] && [ "$widget_exit" -ne 124 ]; then
            widget_exit_ok=false
        fi

        widget_success=true
        if ! $widget_exit_ok; then widget_success=false; fi
        if ! $widget_report_ok; then widget_success=false; fi
        if ! $widget_parse_ok; then widget_success=false; fi

        echo "Outcome: $widget_outcome"
        echo "Summary JSONL: $WIDGET_JSONL"

        $widget_success
    } > "$WIDGET_LOG" 2>&1
    widget_exit=$?
    widget_dur_ms=$(( $(e2e_now_ms) - widget_start_ms ))
    widget_dur_s=$(echo "scale=2; $widget_dur_ms / 1000" | bc 2>/dev/null || echo "${widget_dur_ms}ms")
    STEP_DURATIONS+=("${widget_dur_s}s")

    if [ $widget_exit -eq 0 ]; then
        log_pass "Widget builder export passed in ${widget_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "widget builder export" "success" "$widget_dur_ms"
    else
        log_fail "Widget builder export failed. See: $WIDGET_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "widget builder export" "failed" "$widget_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 15: Determinism Lab JSONL (bd-iuvb.2)
    #
    # Runs the Determinism Lab (screen 37) and exports JSONL verification data.
    # ────────────────────────────────────────────────────────────────────────
    log_step "determinism lab report (screen 37)"
    log_info "Running Determinism Lab and validating JSONL..."
    DET_LOG="$LOG_DIR/15_determinism_lab.log"
    DET_REPORT="$LOG_DIR/15_determinism_report_${TIMESTAMP}.jsonl"
    DET_JSONL="$LOG_DIR/15_determinism_summary.jsonl"
    STEP_NAMES+=("determinism lab report")

    jsonl_step_start "determinism lab report"
    det_start_ms="$(e2e_now_ms)"
    {
        echo "=== Determinism Lab (Screen 37) ==="
        echo "Bead: bd-iuvb.2"
        echo "Report path: $DET_REPORT"
        echo ""

        det_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.6; printf 'e' > /dev/tty) & FTUI_DETERMINISM_LAB_REPORT=\"$DET_REPORT\" FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=37 timeout 8 $DEMO_BIN"

        if run_in_pty "$det_cmd" 2>&1; then
            det_run_exit=0
        else
            det_run_exit=$?
        fi

        if [ "$det_run_exit" -eq 124 ]; then
            det_outcome="timeout"
        elif [ "$det_run_exit" -eq 0 ]; then
            det_outcome="pass"
        else
            det_outcome="fail"
        fi

        det_report_ok=false
        if [ -s "$DET_REPORT" ]; then
            det_report_ok=true
        else
            echo "Report file missing or empty: $DET_REPORT"
            det_outcome="no_report"
        fi

        det_parse_ok=false
        if $det_report_ok; then
            if python3 - "$DET_REPORT" "$DET_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$det_outcome" "$det_run_exit" <<'PY'
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

lines = []
with open(report_path, "r", encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if not line:
            continue
        lines.append(json.loads(line))

required = {
    "event",
    "timestamp",
    "run_id",
    "hash_key",
    "frame",
    "seed",
    "width",
    "height",
    "strategy",
    "checksum",
    "changes",
    "mismatch_count",
}
strategies = set()
missing = 0
env_missing = 0
env_seen = 0
for entry in lines:
    if entry.get("event") != "determinism_report":
        if entry.get("event") == "determinism_env":
            env_seen += 1
            env_required = {"event", "timestamp", "run_id", "hash_key", "seed", "width", "height", "env"}
            if not env_required.issubset(entry.keys()):
                env_missing += 1
        continue
    strategies.add(entry.get("strategy"))
    if not required.issubset(entry.keys()):
        missing += 1

ok = len(strategies) >= 3 and missing == 0 and env_seen >= 1 and env_missing == 0 and len(lines) >= 3

try:
    seed_val = int(seed)
except ValueError:
    seed_val = None
summary = {
    "schema_version": schema_version,
    "type": "determinism_summary",
    "timestamp": timestamp,
    "run_id": run_id,
    "seed": seed_val,
    "outcome": outcome,
    "exit_code": int(exit_code),
    "line_count": len(lines),
    "strategy_count": len(strategies),
    "strategies": sorted([s for s in strategies if s]),
    "missing_required": missing,
    "env_seen": env_seen,
    "env_missing_required": env_missing,
}

with open(summary_path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(summary) + "\\n")

print(json.dumps(summary))
sys.exit(0 if ok else 2)
PY
            then
                det_parse_ok=true
            else
                det_parse_ok=false
            fi
        fi

        det_exit_ok=true
        if [ "$det_run_exit" -ne 0 ] && [ "$det_run_exit" -ne 124 ]; then
            det_exit_ok=false
        fi

        det_success=true
        if ! $det_exit_ok; then det_success=false; fi
        if ! $det_report_ok; then det_success=false; fi
        if ! $det_parse_ok; then det_success=false; fi

        echo "Outcome: $det_outcome"
        echo "Summary JSONL: $DET_JSONL"

        $det_success
    } > "$DET_LOG" 2>&1
    det_exit=$?
    det_dur_ms=$(( $(e2e_now_ms) - det_start_ms ))
    det_dur_s=$(echo "scale=2; $det_dur_ms / 1000" | bc 2>/dev/null || echo "${det_dur_ms}ms")
    STEP_DURATIONS+=("${det_dur_s}s")

    if [ $det_exit -eq 0 ]; then
        log_pass "Determinism lab report passed in ${det_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "determinism lab report" "success" "$det_dur_ms"
    else
        log_fail "Determinism lab report failed. See: $DET_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "determinism lab report" "failed" "$det_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 16: Hyperlink Playground JSONL (bd-iuvb.14)
    #
    # Runs the Hyperlink Playground (screen 38) and captures JSONL events.
    # ────────────────────────────────────────────────────────────────────────
    log_step "hyperlink playground (screen 38)"
    log_info "Running Hyperlink Playground and validating JSONL..."
    LINK_LOG="$LOG_DIR/16_hyperlink_playground.log"
    LINK_REPORT="$LOG_DIR/16_hyperlink_report_${TIMESTAMP}.jsonl"
    LINK_JSONL="$LOG_DIR/16_hyperlink_summary.jsonl"
    STEP_NAMES+=("hyperlink playground")

    jsonl_step_start "hyperlink playground"
    link_start_ms="$(e2e_now_ms)"
    {
        echo "=== Hyperlink Playground (Screen 38) ==="
        echo "Bead: bd-iuvb.14"
        echo "Report path: $LINK_REPORT"
        echo ""

        link_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.5; printf '\\t\\r' > /dev/tty) & FTUI_LINK_REPORT_PATH=\"$LINK_REPORT\" FTUI_LINK_RUN_ID=\"$RUN_ID\" FTUI_DEMO_EXIT_AFTER_MS=2200 FTUI_DEMO_SCREEN=38 timeout 8 $DEMO_BIN"

        if run_in_pty "$link_cmd" 2>&1; then
            link_exit=0
        else
            link_exit=$?
        fi

        if [ "$link_exit" -eq 124 ]; then
            link_outcome="timeout"
        elif [ "$link_exit" -eq 0 ]; then
            link_outcome="pass"
        else
            link_outcome="fail"
        fi

        link_report_ok=false
        if [ -s "$LINK_REPORT" ]; then
            link_report_ok=true
        else
            echo "Report file missing or empty: $LINK_REPORT"
            link_outcome="no_report"
        fi

        link_parse_ok=false
        if $link_report_ok; then
            if python3 - "$LINK_REPORT" "$LINK_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$link_outcome" "$link_exit" <<'PY'
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

with open(report_path, "r", encoding="utf-8") as handle:
    lines = [line for line in handle if line.strip()]
if not lines:
    raise SystemExit("Report JSONL is empty")

events = []
for line in lines:
    data = json.loads(line)
    for key in ("run_id", "link_id", "focus_idx", "action", "outcome"):
        if key not in data:
            raise SystemExit(f"Missing key: {key}")
    events.append(data)

try:
    seed_val = int(seed)
except ValueError:
    seed_val = None
payload = {
    "schema_version": schema_version,
    "type": "hyperlink_summary",
    "timestamp": timestamp,
    "run_id": run_id,
    "seed": seed_val,
    "event_count": len(events),
    "actions": sorted({evt["action"] for evt in events}),
    "outcome": outcome,
    "exit_code": int(exit_code),
}

with open(summary_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\n")
PY
            then
                link_parse_ok=true
            else
                echo "Failed to parse hyperlink report into summary JSONL"
                link_outcome="parse_fail"
            fi
        fi

        link_exit_ok=true
        if [ "$link_exit" -ne 0 ] && [ "$link_exit" -ne 124 ]; then
            link_exit_ok=false
        fi

        link_success=true
        if ! $link_exit_ok; then link_success=false; fi
        if ! $link_report_ok; then link_success=false; fi
        if ! $link_parse_ok; then link_success=false; fi

        echo "Outcome: $link_outcome"
        echo "Summary JSONL: $LINK_JSONL"

        $link_success
    } > "$LINK_LOG" 2>&1
    link_exit=$?
    link_dur_ms=$(( $(e2e_now_ms) - link_start_ms ))
    link_dur_s=$(echo "scale=2; $link_dur_ms / 1000" | bc 2>/dev/null || echo "${link_dur_ms}ms")
    STEP_DURATIONS+=("${link_dur_s}s")

    if [ $link_exit -eq 0 ]; then
        log_pass "Hyperlink playground passed in ${link_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "hyperlink playground" "success" "$link_dur_ms"
    else
        log_fail "Hyperlink playground failed. See: $LINK_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "hyperlink playground" "failed" "$link_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 17: Command Palette JSONL (bd-iuvb.16)
    #
    # Opens the palette, runs a query, executes an action, toggles favorite,
    # and emits JSONL diagnostics for E2E verification.
    # ────────────────────────────────────────────────────────────────────────
    log_step "command palette (bd-iuvb.16)"
    log_info "Running command palette flow and validating JSONL..."
    PAL_LOG="$LOG_DIR/17_palette.log"
    PAL_REPORT="$LOG_DIR/17_palette_report_${TIMESTAMP}.jsonl"
    PAL_JSONL="$LOG_DIR/17_palette_summary.jsonl"
    STEP_NAMES+=("command palette")

    jsonl_step_start "command palette"
    pal_start_ms="$(e2e_now_ms)"
    {
        echo "=== Command Palette (bd-iuvb.16) ==="
        echo "Report path: $PAL_REPORT"
        echo ""

        pal_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.5; printf '\\x0b' > /dev/tty; sleep 0.2; printf 'dash' > /dev/tty; sleep 0.2; printf '\\r' > /dev/tty; sleep 0.3; printf '\\x0b' > /dev/tty; sleep 0.2; printf '\\x06' > /dev/tty; sleep 0.2; printf '\\x1b' > /dev/tty) & FTUI_PALETTE_REPORT_PATH=\"$PAL_REPORT\" FTUI_PALETTE_RUN_ID=\"$RUN_ID\" FTUI_DEMO_EXIT_AFTER_MS=2400 FTUI_DEMO_SCREEN=1 timeout 8 $DEMO_BIN"

        if run_in_pty "$pal_cmd" 2>&1; then
            pal_exit=0
        else
            pal_exit=$?
        fi

        if [ "$pal_exit" -eq 124 ]; then
            pal_outcome="timeout"
        elif [ "$pal_exit" -eq 0 ]; then
            pal_outcome="pass"
        else
            pal_outcome="fail"
        fi

        pal_report_ok=false
        if [ -s "$PAL_REPORT" ]; then
            pal_report_ok=true
        else
            echo "Report file missing or empty: $PAL_REPORT"
            pal_outcome="no_report"
        fi

        pal_parse_ok=false
        if $pal_report_ok; then
            if python3 - "$PAL_REPORT" "$PAL_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$pal_outcome" "$pal_exit" <<'PY'
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

with open(report_path, "r", encoding="utf-8") as handle:
    lines = [line for line in handle if line.strip()]
if not lines:
    raise SystemExit("Report JSONL is empty")

required = {"run_id", "action", "query", "selected_screen", "category", "outcome"}
missing_required = 0
actions = set()
for line in lines:
    entry = json.loads(line)
    actions.add(entry.get("action"))
    if not required.issubset(entry.keys()):
        missing_required += 1

ok = len(lines) >= 2 and missing_required == 0 and ("execute" in actions)

try:
    seed_val = int(seed)
except ValueError:
    seed_val = None
summary = {
    "schema_version": schema_version,
    "type": "command_palette_summary",
    "timestamp": timestamp,
    "run_id": run_id,
    "seed": seed_val,
    "outcome": outcome,
    "exit_code": int(exit_code),
    "line_count": len(lines),
    "actions": sorted([a for a in actions if a]),
    "missing_required": missing_required,
}

with open(summary_path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(summary) + "\\n")

print(json.dumps(summary))
sys.exit(0 if ok else 2)
PY
            then
                pal_parse_ok=true
            else
                pal_parse_ok=false
            fi
        fi

        pal_exit_ok=true
        if [ "$pal_exit" -ne 0 ] && [ "$pal_exit" -ne 124 ]; then
            pal_exit_ok=false
        fi

        pal_success=true
        if ! $pal_exit_ok; then pal_success=false; fi
        if ! $pal_report_ok; then pal_success=false; fi
        if ! $pal_parse_ok; then pal_success=false; fi

        echo "Outcome: $pal_outcome"
        echo "Summary JSONL: $PAL_JSONL"

        $pal_success
    } > "$PAL_LOG" 2>&1
    pal_exit=$?
    pal_dur_ms=$(( $(e2e_now_ms) - pal_start_ms ))
    pal_dur_s=$(echo "scale=2; $pal_dur_ms / 1000" | bc 2>/dev/null || echo "${pal_dur_ms}ms")
    STEP_DURATIONS+=("${pal_dur_s}s")

    if [ $pal_exit -eq 0 ]; then
        log_pass "Command palette flow passed in ${pal_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "command palette" "success" "$pal_dur_ms"
    else
        log_fail "Command palette flow failed. See: $PAL_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "command palette" "failed" "$pal_dur_ms"
    fi

    # ────────────────────────────────────────────────────────────────────────
    # Step 18: Explainability cockpit evidence JSONL (bd-iuvb.4)
    #
    # Runs the Explainability Cockpit screen with evidence logging enabled,
    # triggers a few resize events, and summarizes diff/resize/budget evidence.
    # ────────────────────────────────────────────────────────────────────────
    log_step "explainability cockpit (bd-iuvb.4)"
    log_info "Running explainability cockpit and validating evidence JSONL..."
    EXPLAIN_LOG="$LOG_DIR/18_explainability.log"
    EXPLAIN_REPORT="$LOG_DIR/18_explainability_evidence_${TIMESTAMP}.jsonl"
    EXPLAIN_JSONL="$LOG_DIR/18_explainability_summary.jsonl"
    STEP_NAMES+=("explainability cockpit")

    jsonl_step_start "explainability cockpit"
    explain_start_ms="$(e2e_now_ms)"
    {
        echo "=== Explainability Cockpit (bd-iuvb.4) ==="
        echo "Evidence path: $EXPLAIN_REPORT"
        echo ""

        explain_cmd="stty rows 24 cols 80 2>/dev/null; (sleep 0.6; stty rows 30 cols 100 2>/dev/null; sleep 0.6; stty rows 24 cols 80 2>/dev/null; sleep 0.6; stty rows 35 cols 120 2>/dev/null) & FTUI_DEMO_EVIDENCE_JSONL=\"$EXPLAIN_REPORT\" FTUI_DEMO_EXIT_AFTER_MS=3200 FTUI_DEMO_SCREEN=40 timeout 10 $DEMO_BIN"

        if run_in_pty "$explain_cmd" 2>&1; then
            explain_exit=0
        else
            explain_exit=$?
        fi

        if [ "$explain_exit" -eq 124 ]; then
            explain_outcome="timeout"
        elif [ "$explain_exit" -eq 0 ]; then
            explain_outcome="pass"
        else
            explain_outcome="fail"
        fi

        explain_report_ok=false
        if [ -s "$EXPLAIN_REPORT" ]; then
            explain_report_ok=true
        else
            echo "Evidence file missing or empty: $EXPLAIN_REPORT"
            explain_outcome="no_report"
        fi

        explain_parse_ok=false
        if $explain_report_ok; then
            if python3 - "$EXPLAIN_REPORT" "$EXPLAIN_JSONL" "$RUN_ID" "${E2E_SEED:-0}" "$(e2e_timestamp)" "${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}" "$explain_outcome" "$explain_exit" <<'PY'
import hashlib
import json
import sys

report_path, summary_path, run_id, seed, timestamp, schema_version, outcome, exit_code = sys.argv[1:9]

with open(report_path, "r", encoding="utf-8") as handle:
    lines = [line for line in handle if line.strip()]
if not lines:
    raise SystemExit("Evidence JSONL is empty")

def stable_hash(payload):
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()[:16]

def summarize_diff(entry):
    return {
        "event": "diff_decision",
        "strategy": entry.get("strategy"),
        "posterior_mean": entry.get("posterior_mean"),
        "posterior_variance": entry.get("posterior_variance"),
        "guard_reason": entry.get("guard_reason"),
        "fallback_reason": entry.get("fallback_reason"),
        "hysteresis_applied": entry.get("hysteresis_applied"),
        "hysteresis_ratio": entry.get("hysteresis_ratio"),
        "dirty_rows": entry.get("dirty_rows"),
        "total_rows": entry.get("total_rows"),
    }

def summarize_resize(entry):
    return {
        "event": "resize_decision",
        "regime": entry.get("regime"),
        "action": entry.get("action"),
        "forced": entry.get("forced"),
        "pending_w": entry.get("pending_w"),
        "pending_h": entry.get("pending_h"),
        "applied_w": entry.get("applied_w"),
        "applied_h": entry.get("applied_h"),
    }

def summarize_budget(entry):
    return {
        "event": "budget_decision",
        "decision": entry.get("decision"),
        "decision_controller": entry.get("decision_controller"),
        "degradation_before": entry.get("degradation_before"),
        "degradation_after": entry.get("degradation_after"),
        "in_warmup": entry.get("in_warmup"),
        "bucket_key": entry.get("bucket_key"),
    }

summaries = []
seen_types = set()

for line in lines:
    entry = json.loads(line)
    event = entry.get("event")
    if event == "diff_decision":
        frame_idx = entry.get("event_idx")
        summary = summarize_diff(entry)
        decision_type = "diff_decision"
    elif event == "decision" and "regime" in entry and "action" in entry:
        frame_idx = entry.get("event_idx", entry.get("idx"))
        summary = summarize_resize(entry)
        decision_type = "resize_decision"
    elif event == "budget_decision":
        frame_idx = entry.get("frame_idx")
        summary = summarize_budget(entry)
        decision_type = "budget_decision"
    else:
        continue

    if frame_idx is None:
        continue

    seen_types.add(decision_type)
    summaries.append({
        "schema_version": schema_version,
        "type": "explainability_summary",
        "timestamp": timestamp,
        "run_id": run_id,
        "decision_type": decision_type,
        "frame_idx": int(frame_idx),
        "evidence_summary_hash": stable_hash(summary),
        "outcome": outcome,
        "exit_code": int(exit_code),
    })

required = {"diff_decision", "resize_decision", "budget_decision"}
missing = required - seen_types

with open(summary_path, "w", encoding="utf-8") as handle:
    for summary in summaries:
        handle.write(json.dumps(summary) + "\n")

if missing:
    raise SystemExit(f"Missing decision types: {sorted(missing)}")
PY
            then
                explain_parse_ok=true
            else
                explain_parse_ok=false
            fi
        fi

        explain_exit_ok=true
        if [ "$explain_exit" -ne 0 ] && [ "$explain_exit" -ne 124 ]; then
            explain_exit_ok=false
        fi

        explain_success=true
        if ! $explain_exit_ok; then explain_success=false; fi
        if ! $explain_report_ok; then explain_success=false; fi
        if ! $explain_parse_ok; then explain_success=false; fi

        echo "Outcome: $explain_outcome"
        echo "Summary JSONL: $EXPLAIN_JSONL"

        $explain_success
    } > "$EXPLAIN_LOG" 2>&1
    explain_exit=$?
    explain_dur_ms=$(( $(e2e_now_ms) - explain_start_ms ))
    explain_dur_s=$(echo "scale=2; $explain_dur_ms / 1000" | bc 2>/dev/null || echo "${explain_dur_ms}ms")
    STEP_DURATIONS+=("${explain_dur_s}s")

    if [ $explain_exit -eq 0 ]; then
        log_pass "Explainability cockpit passed in ${explain_dur_s}s"
        PASS_COUNT=$((PASS_COUNT + 1))
        STEP_STATUSES+=("PASS")
        jsonl_step_end "explainability cockpit" "success" "$explain_dur_ms"
    else
        log_fail "Explainability cockpit failed. See: $EXPLAIN_LOG"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        STEP_STATUSES+=("FAIL")
        jsonl_step_end "explainability cockpit" "failed" "$explain_dur_ms"
    fi

else
    # No PTY support — skip all smoke/interactive tests
    for step in "Smoke test (alt-screen)" "Smoke test (inline)" \
                "Screen navigation" "Search test (Shakespeare)" \
                "Resize (SIGWINCH) test" "VisualEffects backdrop" \
                "Layout Inspector" "Terminal caps report" "i18n stress report" \
                "Widget builder export" "Determinism lab report" \
                "Hyperlink playground" "command palette" "explainability cockpit"; do
        skip_step "$step" "$SMOKE_REASON"
    done
fi

fi  # end of non-quick block

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=============================================="
echo "  E2E Test Suite Complete"
echo "=============================================="
echo ""
echo "Ended at: $(e2e_timestamp)"
echo "Log directory: $LOG_DIR"
echo ""

# Summary table
printf "%-35s %-6s %s\n" "Step" "Status" "Duration"
printf "%-35s %-6s %s\n" "---" "------" "--------"
for i in "${!STEP_NAMES[@]}"; do
    local_status="${STEP_STATUSES[$i]}"
    case $local_status in
        PASS) color="\033[32m" ;;
        FAIL) color="\033[31m" ;;
        SKIP) color="\033[33m" ;;
        *)    color="" ;;
    esac
    printf "%-35s ${color}%-6s\033[0m %s\n" "${STEP_NAMES[$i]}" "$local_status" "${STEP_DURATIONS[$i]}"
done

echo ""
echo "Results: $PASS_COUNT passed, $FAIL_COUNT failed, $SKIP_COUNT skipped"
echo ""

# List log files with sizes
echo "Log files:"
ls -lh "$LOG_DIR"/*.log 2>/dev/null | awk '{print "  " $9 " (" $5 ")"}'
echo ""

# Generate summary file
{
    echo "Demo Showcase E2E Summary"
    echo "========================="
    echo "Date: $(e2e_timestamp)"
    echo "Passed: $PASS_COUNT"
    echo "Failed: $FAIL_COUNT"
    echo "Skipped: $SKIP_COUNT"
    echo ""
    for i in "${!STEP_NAMES[@]}"; do
        printf "  %-35s %s  %s\n" "${STEP_NAMES[$i]}" "${STEP_STATUSES[$i]}" "${STEP_DURATIONS[$i]}"
    done
    echo ""
    echo "Exit code: $( [ $FAIL_COUNT -eq 0 ] && echo 0 || echo 1 )"
} > "$LOG_DIR/SUMMARY.txt"

jsonl_assert "artifact_summary_txt" "pass" "summary_txt=$LOG_DIR/SUMMARY.txt"
run_duration_ms=$(( $(e2e_now_ms) - E2E_RUN_START_MS ))
if [ $FAIL_COUNT -eq 0 ]; then
    jsonl_run_end "complete" "$run_duration_ms" "$FAIL_COUNT"
else
    jsonl_run_end "failed" "$run_duration_ms" "$FAIL_COUNT"
fi

if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "\033[1;32mAll tests passed!\033[0m"
    exit 0
else
    echo -e "\033[1;31m$FAIL_COUNT test(s) failed!\033[0m"
    exit 1
fi
