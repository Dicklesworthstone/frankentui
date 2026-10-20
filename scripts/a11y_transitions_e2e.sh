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
# under a PTY with the evidence sink enabled, moves the panel focus across
# Form -> SearchInput -> PasswordInput -> SearchInput on separate frames, and
# verifies the FocusChanged node_id sequence [id1, id2, id1] with id1 != id2
# and no assertive urgency.
a11y_focus_announcements_pty() {
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

    local forms_screen="7"
    if declare -f e2e_demo_screen >/dev/null 2>&1; then
        forms_screen="$(e2e_demo_screen "$bin" "forms_input" 2>/dev/null || echo 7)"
    fi

    # Ctrl+Right (CSI 1;5C) at 500ms and 1000ms, then Ctrl+Left (CSI 1;5D) at 1500ms:
    # panel focus moves Form -> SearchInput (id1) -> PasswordInput (id2) -> SearchInput (id1).
    FTUI_DEMO_EVIDENCE_JSONL="$evidence" \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SCREEN="$forms_screen" \
    FTUI_DEMO_EXIT_AFTER_MS=2400 \
    PTY_SEND_SEQUENCE='[{"delay_ms":500,"text":"\u001b[1;5C"},{"delay_ms":1000,"text":"\u001b[1;5C"},{"delay_ms":1500,"text":"\u001b[1;5D"}]' \
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
focus_rows = []
assertive_rows = []
with open(path, encoding="utf-8") as handle:
    for lineno, line in enumerate(handle, start=1):
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"invalid JSON on line {lineno}: {exc}") from exc
        event = row.get("event")
        if event == "a11y_tree":
            trees += 1
        elif event == "a11y_announcement":
            announcements += 1
            if row.get("urgency") == "assertive":
                assertive_rows.append(row)
            if row.get("reason") == "FocusChanged":
                focus_rows.append(row)

post_initial_ids = [row.get("node_id") for row in focus_rows[1:]]
print(
    f"a11y_tree={trees} a11y_announcement={announcements} "
    f"focus_changed={len(focus_rows)} post_initial_ids={post_initial_ids}"
)
if trees == 0:
    raise SystemExit("no a11y_tree rows: the runtime did not build an accessibility tree")
if assertive_rows:
    raise SystemExit(f"unexpected assertive announcements during focus moves: {assertive_rows}")
if len(post_initial_ids) < 3:
    raise SystemExit(
        f"expected >= 3 post-initial FocusChanged announcements [id1, id2, id1], got {post_initial_ids}"
    )
id1, id2, id3 = post_initial_ids[0], post_initial_ids[1], post_initial_ids[2]
if id1 is None or id2 is None or id3 is None:
    raise SystemExit(f"FocusChanged node_id must not be null: {post_initial_ids}")
if id1 != id3:
    raise SystemExit(f"expected first and third focus node_id to match ([id1, id2, id1]), got {post_initial_ids}")
if id1 == id2:
    raise SystemExit(f"expected second focus node_id to differ from first ([id1, id2, id1]), got {post_initial_ids}")
PY
}

run_pty_focus_announcements() {
    a11y_focus_announcements_pty "$@"
}

# G09: PTY-backed proof that the Accessibility Panel screen renders the live
# accessibility tree with node count >= 1 and role lines.
a11y_panel_shows_tree_pty() {
    local bin="$1"
    local pty_out="$E2E_LOG_DIR/a11y_panel_pty.out"

    if [[ ! -x "$bin" ]]; then
        echo "showcase binary not found: $bin" >&2
        return 1
    fi
    if ! declare -f pty_run >/dev/null 2>&1; then
        echo "tests/e2e/lib/pty.sh is required for the PTY scenario" >&2
        return 1
    fi

    local panel_screen="42"
    if declare -f e2e_demo_screen >/dev/null 2>&1; then
        panel_screen="$(e2e_demo_screen "$bin" "accessibility_panel" 2>/dev/null || echo 42)"
    fi

    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SCREEN="$panel_screen" \
    FTUI_DEMO_EXIT_AFTER_MS=1500 \
    PTY_TIMEOUT=6 \
    PTY_COLS=100 \
    PTY_ROWS=30 \
        pty_run "$pty_out" "$bin" || {
        echo "showcase exited abnormally on accessibility_panel under PTY (see $pty_out)" >&2
        return 1
    }

    if [[ ! -s "$pty_out" ]]; then
        echo "no PTY output captured in $pty_out" >&2
        return 1
    fi

    "$E2E_PYTHON" - "$pty_out" <<'PY'
import re
import sys

path = sys.argv[1]
raw = open(path, "rb").read().decode("utf-8", errors="replace")
# Replay ANSI cursor movements onto a 2D cell grid so diff-updated spans
# (e.g. "nod\x1b[2C=9" overwriting "(tree not yet built...") reconstruct the
# actual visible terminal screen.
rows, cols = 40, 140
grid = [[" "] * cols for _ in range(rows)]
r, c = 0, 0
token_re = re.compile(r"\x1b\[([0-9;?]*)([ -/]*[@-~])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-_]")
pos = 0
for m in token_re.finditer(raw):
    chunk = raw[pos:m.start()]
    for ch in chunk:
        if ch == "\r":
            c = 0
        elif ch == "\n":
            r = min(rows - 1, r + 1)
        elif ord(ch) >= 32:
            if 0 <= r < rows and 0 <= c < cols:
                grid[r][c] = ch
            c = min(cols - 1, c + 1)
    params, final = m.group(1), m.group(2)
    if final is not None:
        if final in ("H", "f"):
            parts = [int(p) if p.isdigit() else 1 for p in (params or "").split(";")]
            r = max(0, min(rows - 1, (parts[0] if parts else 1) - 1))
            c = max(0, min(cols - 1, (parts[1] if len(parts) > 1 else 1) - 1))
        elif final == "C":
            n = int(params) if (params and params.isdigit()) else 1
            c = max(0, min(cols - 1, c + n))
        elif final == "D":
            n = int(params) if (params and params.isdigit()) else 1
            c = max(0, c - n)
        elif final == "J" and params == "2":
            grid = [[" "] * cols for _ in range(rows)]
            r, c = 0, 0
    pos = m.end()
for ch in raw[pos:]:
    if ch == "\r":
        c = 0
    elif ch == "\n":
        r = min(rows - 1, r + 1)
    elif ord(ch) >= 32:
        if 0 <= r < rows and 0 <= c < cols:
            grid[r][c] = ch
        c = min(cols - 1, c + 1)

clean = "\n".join("".join(row).rstrip() for row in grid)
match = re.search(r"nodes=(\d+)", clean)
if not match:
    raise SystemExit(f"Accessibility Tree header 'nodes=<n>' not found in PTY screen:\n{clean}")
node_count = int(match.group(1))
if node_count < 1:
    raise SystemExit(f"expected nodes >= 1 in Accessibility Tree pane, got nodes={node_count}")

roles = ("Group", "Button", "TextInput", "Label", "ProgressBar", "List", "Table", "Tab")
if not any(role in clean for role in roles):
    raise SystemExit("expected at least one accessibility role line in Accessibility Tree pane")
print(f"accessibility_panel PTY verified: nodes={node_count}")
PY
}


VERBOSE=false
QUICK=false
PTY_ONLY=false

for arg in "$@"; do
    case "$arg" in
        --verbose|-v) VERBOSE=true ;;
        --quick)      QUICK=true ;;
        --pty-only)   QUICK=true; PTY_ONLY=true ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--quick] [--pty-only]"
            echo "  --verbose   Show full output"
            echo "  --quick     Skip compilation, run tests only"
            echo "  --pty-only  Run only PTY accessibility scenarios using prebuilt binary"
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

if ! $PTY_ONLY; then
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
else
    skip_step "a11y_transition_tests"
    skip_step "screen_reader_mirror_policy_tests"
    skip_step "a11y_preference_behavior_tests"
fi

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
run_step "a11y_focus_announcements_pty" \
    a11y_focus_announcements_pty "$SHOWCASE_BIN"
run_step "a11y_panel_shows_tree_pty" \
    a11y_panel_shows_tree_pty "$SHOWCASE_BIN"

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
