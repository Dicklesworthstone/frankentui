#!/bin/bash
# Accessibility Modes Transition E2E Tests (bd-2o55.2)
#
# Runs targeted a11y transition regression tests with JSONL logging.
#
# Usage:
#   ./scripts/a11y_transitions_e2e.sh [--verbose] [--quick]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB_DIR="$PROJECT_ROOT/tests/e2e/lib"
PRESET_E2E_JSONL_FILE="${E2E_JSONL_FILE:-}"

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
# shellcheck source=/dev/null
if [[ -f "$LIB_DIR/pty.sh" ]]; then
    source "$LIB_DIR/pty.sh"
fi

# G09: PTY-backed proof that a running program builds the accessibility tree
# and announces focus changes. Runs the showcase on the Forms & Input screen
# (screen 7) under a PTY with the evidence sink enabled, moves the panel
# focus with Ctrl+Right, and requires `a11y_tree` rows plus
# `a11y_announcement` rows whose reason is `FocusChanged`. The evidence JSONL
# and the PTY transcript stay in the log directory for diagnosis.
run_pty_focus_announcements() {
    local bin="$1"
    local evidence="$E2E_LOG_DIR/a11y_focus_evidence.jsonl"
    local pty_out="$E2E_LOG_DIR/a11y_focus_pty.out"

    if [[ ! -x "$bin" ]]; then
        echo "showcase binary not found: $bin" >&2
        return 1
    fi
    if ! declare -f pty_run >/dev/null 2>&1; then
        echo "tests/e2e/lib/pty.sh is required for the PTY scenario" >&2
        return 1
    fi

    # Ctrl+Right (CSI 1;5C) twice after the first frames have rendered: the
    # screen's panel focus moves Form -> Search input -> Password input, and
    # each TextInput reports `focused` in its accessibility node, so the tree
    # focus changes and the diff announces it. (Tab moves inside the Form
    # widget, which has no accessibility nodes yet.)
    FTUI_DEMO_EVIDENCE_JSONL="$evidence" \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SCREEN=7 \
    FTUI_DEMO_EXIT_AFTER_MS=2500 \
    PTY_SEND=$'\e[1;5C\e[1;5C' \
    PTY_SEND_DELAY_MS=700 \
    PTY_TIMEOUT=8 \
    PTY_COLS=100 \
    PTY_ROWS=30 \
        pty_run "$pty_out" "$bin" || {
        echo "showcase exited abnormally under the PTY (see $pty_out)" >&2
        return 1
    }

    if [[ ! -s "$evidence" ]]; then
        echo "no evidence written to $evidence" >&2
        return 1
    fi

    "$E2E_PYTHON" - "$evidence" <<'PY'
import json
import sys

path = sys.argv[1]
trees = 0
announcements = 0
focus_changed = 0
with open(path, encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        event = row.get("event")
        if event == "a11y_tree":
            trees += 1
        elif event == "a11y_announcement":
            announcements += 1
            if row.get("reason") == "FocusChanged":
                focus_changed += 1
print(f"a11y_tree={trees} a11y_announcement={announcements} focus_changed={focus_changed}")
if trees == 0:
    raise SystemExit("no a11y_tree rows: the runtime did not build an accessibility tree")
if focus_changed == 0:
    raise SystemExit("no FocusChanged announcements after Ctrl+Right panel switches")
PY
}

VERBOSE=false
QUICK=false

for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick)      QUICK=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick]"
            echo "  --verbose  Show full output"
            echo "  --quick    Skip compilation, run tests only"
            exit 0
            ;;
    esac
done

e2e_fixture_init "a11y_transitions"
TIMESTAMP="$(e2e_log_stamp)"
LOG_DIR="${LOG_DIR:-/tmp/ftui-a11y-transitions-${E2E_RUN_ID}-${TIMESTAMP}}"
E2E_LOG_DIR="$LOG_DIR"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$LOG_DIR/results}"
E2E_JSONL_FILE="${A11Y_TRANSITIONS_JSONL_FILE:-${PRESET_E2E_JSONL_FILE:-$LOG_DIR/a11y_transitions_e2e.jsonl}}"
E2E_CHILD_JSONL_DIR="${E2E_CHILD_JSONL_DIR:-$LOG_DIR/child-jsonl}"
E2E_RUN_CMD="${E2E_RUN_CMD:-$0 $*}"
E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
export E2E_LOG_DIR E2E_RESULTS_DIR E2E_JSONL_FILE E2E_RUN_CMD E2E_RUN_START_MS
mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR" "$E2E_CHILD_JSONL_DIR"
jsonl_init
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"
jsonl_assert "child_jsonl_dir" "pass" "child_jsonl_dir=$E2E_CHILD_JSONL_DIR"
jsonl_set_context "host" "${COLUMNS:-}" "${LINES:-}" "${E2E_SEED:-0}"

PASSED=0
FAILED=0
SKIPPED=0

run_step() {
    local name="$1"
    shift
    local step_start
    step_start=$(e2e_now_ms)

    jsonl_step_start "$name"

    local exit_code=0
    local output_file="$LOG_DIR/${name}.log"

    if $VERBOSE; then
        "$@" 2>&1 | tee "$output_file" || exit_code=$?
    else
        "$@" > "$output_file" 2>&1 || exit_code=$?
    fi

    local step_end
    step_end=$(e2e_now_ms)
    local elapsed=$(( step_end - step_start ))

    if [ "$exit_code" -eq 0 ]; then
        PASSED=$((PASSED + 1))
        jsonl_step_end "$name" "success" "$elapsed"
        printf "  %-50s  PASS  (%s ms)\n" "$name" "$elapsed"
    else
        FAILED=$((FAILED + 1))
        jsonl_step_end "$name" "failed" "$elapsed"
        printf "  %-50s  FAIL  (exit %s, %s ms)\n" "$name" "$exit_code" "$elapsed"
        echo "    Log: $output_file"
    fi
}

skip_step() {
    local name="$1"
    SKIPPED=$((SKIPPED + 1))
    jsonl_step_start "$name"
    jsonl_step_end "$name" "skipped" 0
    printf "  %-50s  SKIP\n" "$name"
}

echo "=========================================="
echo " Accessibility Modes Transition E2E (bd-2o55.2)"
echo "=========================================="
echo ""

echo "  Log directory: $LOG_DIR"
echo ""

if ! $QUICK; then
    run_step "cargo_check" \
        cargo check -p ftui-demo-showcase --tests --quiet

    run_step "cargo_clippy" \
        cargo clippy -p ftui-demo-showcase --tests -- -D warnings --quiet
else
    skip_step "cargo_check"
    skip_step "cargo_clippy"
fi

run_step "a11y_transition_tests" bash -c "
    cd '$PROJECT_ROOT' &&
    E2E_JSONL=1 \
    E2E_JSONL_FILE='$E2E_CHILD_JSONL_DIR/a11y_transition_tests.jsonl' \
    A11Y_TEST_SEED=\${A11Y_TEST_SEED:-0} \
        cargo test -p ftui-demo-showcase --test a11y_snapshots -- a11y_transition --nocapture
"

run_step "screen_reader_mirror_policy_tests" bash -c "
    cd '$PROJECT_ROOT' &&
    E2E_JSONL=1 \
    E2E_JSONL_FILE='$E2E_CHILD_JSONL_DIR/screen_reader_mirror_policy_tests.jsonl' \
    A11Y_TEST_SEED=\${A11Y_TEST_SEED:-0} \
        cargo test -p ftui-a11y --test a11y_tests -- screen_reader --nocapture
"

# bd-2vr05.7.4: reduced-motion / high-contrast preference behavior controls.
run_step "a11y_preference_behavior_tests" bash -c "
    cd '$PROJECT_ROOT' &&
    E2E_JSONL=1 \
    E2E_JSONL_FILE='$E2E_CHILD_JSONL_DIR/a11y_preference_behavior_tests.jsonl' \
    A11Y_TEST_SEED=\${A11Y_TEST_SEED:-0} \
        cargo test -p ftui-a11y -- preferences --nocapture
"

# G09: per-frame accessibility tree + focus announcements under a real PTY.
if ! $QUICK; then
    run_step "showcase_build" \
        cargo build -p ftui-demo-showcase --quiet
else
    skip_step "showcase_build"
fi
SHOWCASE_TARGET_DIR=$(cargo metadata --format-version=1 -q 2>/dev/null \
    | "${E2E_PYTHON:-python3}" -c "import sys,json;print(json.load(sys.stdin)['target_directory'])" 2>/dev/null \
    || echo "$PROJECT_ROOT/target")
SHOWCASE_BIN="$SHOWCASE_TARGET_DIR/debug/ftui-demo-showcase"
run_step "pty_focus_announcements" \
    run_pty_focus_announcements "$SHOWCASE_BIN"

echo ""
echo "=========================================="
TOTAL=$((PASSED + FAILED + SKIPPED))
echo "  Total: $TOTAL  Passed: $PASSED  Failed: $FAILED  Skipped: $SKIPPED"
echo "=========================================="
echo ""

run_status="success"
if [ "$FAILED" -ne 0 ]; then
    run_status="failed"
fi
jsonl_run_end "$run_status" "$(( $(e2e_now_ms) - ${E2E_RUN_START_MS:-$(e2e_now_ms)} ))" "$FAILED"

[[ $FAILED -eq 0 ]]
