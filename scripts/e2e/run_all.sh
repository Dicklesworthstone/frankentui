#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LIB_DIR="$SCRIPT_DIR/lib"
RUN_ARGS=("$@")

# shellcheck source=/dev/null
source "$PROJECT_ROOT/tests/e2e/lib/common.sh"
# shellcheck source=/dev/null
source "$PROJECT_ROOT/tests/e2e/lib/logging.sh"

VERBOSE=false
JSON_OUT=""

for arg in "$@"; do
    case "$arg" in
        --verbose|-v)
            VERBOSE=true
            LOG_LEVEL="DEBUG"
            ;;
        --json)
            shift
            JSON_OUT="${1:-}"
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--json <path>]"
            exit 0
            ;;
    esac
    shift || true
    if [[ "$arg" == "--json" ]]; then
        shift || true
    fi
    if [[ "$arg" == "--verbose" || "$arg" == "-v" || "$arg" == "--help" || "$arg" == "-h" ]]; then
        continue
    fi
    if [[ "$arg" == "--json" ]]; then
        continue
    fi
    if [[ "$arg" == "" ]]; then
        continue
    fi
    if [[ "$arg" == --json* ]]; then
        JSON_OUT="${arg#--json=}"
    fi
    if [[ "$arg" == --verbose* ]]; then
        VERBOSE=true
        LOG_LEVEL="DEBUG"
    fi
done

E2E_RUN_CMD="${E2E_RUN_CMD:-$0 ${RUN_ARGS[*]}}"
TIMESTAMP="$(e2e_log_stamp)"
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_${TIMESTAMP}}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-$E2E_LOG_DIR/results}"
LOG_FILE="$E2E_LOG_DIR/e2e.log"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"

export E2E_LOG_DIR E2E_RESULTS_DIR LOG_FILE LOG_LEVEL E2E_JSONL_FILE E2E_RUN_CMD
export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"

mkdir -p "$E2E_LOG_DIR" "$E2E_RESULTS_DIR"
jsonl_init

log_info "FrankenTUI E2E Test Suite"
log_info "Project root: $PROJECT_ROOT"
log_info "Log directory: $E2E_LOG_DIR"
log_info "Results directory: $E2E_RESULTS_DIR"
log_info "Mode: $([ "$VERBOSE" = true ] && echo verbose || echo normal)"

# Environment info
{
    echo "Environment Information"
    echo "======================="
    echo "Date: $(e2e_timestamp)"
    echo "User: $(whoami)"
    echo "Hostname: $(hostname)"
    echo "Working directory: $(pwd)"
    echo "Rust version: $(rustc --version 2>/dev/null || echo 'N/A')"
    echo "Cargo version: $(cargo --version 2>/dev/null || echo 'N/A')"
    echo "Git status:"
    git status --short 2>/dev/null || echo "Not a git repo"
    echo "Git commit:"
    git log -1 --oneline 2>/dev/null || echo "N/A"
} > "$E2E_LOG_DIR/00_environment.log"

require_cmd cargo
if [[ -z "$E2E_PYTHON" ]]; then
    log_error "python3/python is required for PTY helpers"
    exit 1
fi

log_info "Building ftui-harness..."
if $VERBOSE; then
    cargo build -p ftui-harness | tee "$E2E_LOG_DIR/01_build.log"
else
    cargo build -p ftui-harness > "$E2E_LOG_DIR/01_build.log" 2>&1
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
E2E_HARNESS_BIN="${E2E_HARNESS_BIN:-$TARGET_DIR/debug/ftui-harness}"
export E2E_HARNESS_BIN

if ! e2e_require bin "$E2E_HARNESS_BIN" "ftui-harness binary not found at $E2E_HARNESS_BIN"; then
    log_error "ftui-harness binary not found at $E2E_HARNESS_BIN"
    exit 2
fi

run_group() {
    local dir="$1"
    if [[ ! -d "$dir" ]]; then
        return 0
    fi
    for script in "$dir"/*.sh; do
        if [[ -f "$script" ]]; then
            "$script"
        fi
    done
}

log_info "Running tests..."
run_group "$SCRIPT_DIR/render"
run_group "$SCRIPT_DIR/layout"
run_group "$SCRIPT_DIR/capability_sim"
run_group "$SCRIPT_DIR/widgets"
run_group "$SCRIPT_DIR/input"
run_group "$SCRIPT_DIR/integration"
run_group "$SCRIPT_DIR/modal_dialog"
run_group "$SCRIPT_DIR/i18n"

SUMMARY_JSON="$E2E_RESULTS_DIR/summary.json"
finalize_summary "$SUMMARY_JSON"

if [[ -n "$JSON_OUT" ]]; then
    cp "$SUMMARY_JSON" "$JSON_OUT"
fi

JUNIT_XML="$E2E_RESULTS_DIR/junit.xml"
"$E2E_PYTHON" - <<'PY' "$SUMMARY_JSON" "$JUNIT_XML"
import json
import sys
from datetime import datetime

summary_path = sys.argv[1]
output_path = sys.argv[2]

with open(summary_path, "r", encoding="utf-8") as handle:
    data = json.load(handle)

cases = data.get("tests", [])
failed = data.get("failed", 0)
skipped = data.get("skipped", 0)

total = data.get("total", len(cases))

suite_name = "ftui-e2e"

def esc(text):
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("\"", "&quot;")
    )

lines = []
lines.append(f'<?xml version="1.0" encoding="UTF-8"?>')
lines.append(
    f'<testsuite name="{suite_name}" tests="{total}" failures="{failed}" skipped="{skipped}" timestamp="{datetime.utcnow().isoformat()}">'
)

for case in cases:
    name = esc(case.get("name", "unknown"))
    duration = case.get("duration_ms", 0) / 1000.0
    status = case.get("status", "unknown")
    lines.append(f'  <testcase name="{name}" time="{duration:.3f}">')
    if status == "failed":
        msg = esc(case.get("error", "failed"))
        lines.append(f'    <failure message="{msg}"></failure>')
    elif status == "skipped":
        reason = esc(case.get("error", "skipped"))
        lines.append(f'    <skipped message="{reason}"></skipped>')
    lines.append("  </testcase>")

lines.append("</testsuite>")

with open(output_path, "w", encoding="utf-8") as handle:
    handle.write("\n".join(lines))
PY

log_info "E2E summary: $SUMMARY_JSON"
log_info "JUnit XML: $JUNIT_XML"
log_info "E2E logs: $E2E_LOG_DIR"

FAIL_COUNT=$("$E2E_PYTHON" - <<'PY' "$SUMMARY_JSON"
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    data = json.load(handle)
print(int(data.get("failed", 0)))
PY
)

SKIP_COUNT=$("$E2E_PYTHON" - <<'PY' "$SUMMARY_JSON"
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    data = json.load(handle)
print(int(data.get("skipped", 0)))
PY
)

TOTAL_COUNT=$("$E2E_PYTHON" - <<'PY' "$SUMMARY_JSON"
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    data = json.load(handle)
print(int(data.get("total", 0)))
PY
)

if [[ "$FAIL_COUNT" -gt 0 ]]; then
    exit 1
fi

if [[ "$SKIP_COUNT" -gt 0 && "${E2E_ALLOW_SKIPS:-0}" != "1" ]]; then
    exit 2
fi

if [[ "$TOTAL_COUNT" -eq 0 ]]; then
    exit 2
fi

exit 0
