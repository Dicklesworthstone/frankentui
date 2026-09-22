#!/bin/bash
# =============================================================================
# test_telemetry.sh - E2E test for OTEL telemetry export (bd-1z02.9)
# =============================================================================
#
# Purpose:
# - Start a local OTEL HTTP receiver
# - Run harness with telemetry enabled
# - Capture and validate exported spans
# - Log environment, timings, and results in JSONL format
#
# Usage:
#   ./test_telemetry.sh [--verbose] [--skip-build]
#
# Exit codes:
#   0 - All tests passed
#   1 - Test failure (missing spans)
#   2 - Setup/runtime error
#   3 - Skipped (missing dependencies)
#
# JSONL Schema (per bd-1z02.9 requirements):
# - run_id: unique identifier for this test run
# - case: test case name
# - env: terminal environment (TERM, COLORTERM, etc.)
# - seed: deterministic seed if applicable
# - timings: start_ms, end_ms, duration_ms
# - checksums: output/spans checksums
# - capabilities: environment + tooling capabilities
# - outcome: passed/failed/skipped with reason
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

# shellcheck source=/dev/null
[[ -f "$LIB_DIR/common.sh" ]] && source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
[[ -f "$LIB_DIR/logging.sh" ]] && source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
[[ -f "$LIB_DIR/pty.sh" ]] && source "$LIB_DIR/pty.sh"

# =============================================================================
# Configuration
# =============================================================================

VERBOSE=false
SKIP_BUILD=false
LOG_LEVEL="${LOG_LEVEL:-INFO}"

E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-/tmp/ftui_telemetry_e2e}"
RECEIVER_PORT="${RECEIVER_PORT:-14318}"
RECEIVER_LOG="$E2E_RESULTS_DIR/receiver.log"
SPANS_FILE="$E2E_RESULTS_DIR/captured_spans.jsonl"
TEST_TIMEOUT="${TEST_TIMEOUT:-30}"
RUN_ID=""
LOG_JSONL=""
SEED="${SEED:-1337}"
CASE="telemetry_e2e"
DETERMINISTIC="${TELEMETRY_DETERMINISTIC:-1}"
CAP_OTEL_RECEIVER=false

# Required spans per telemetry-events.md spec
REQUIRED_SPANS=(
    "ftui.program.init"
    "ftui.program.view"
    "ftui.render.frame"
    "ftui.render.present"
)

# =============================================================================
# Argument parsing
# =============================================================================

for arg in "$@"; do
    case "$arg" in
        --verbose|-v)
            VERBOSE=true
            LOG_LEVEL="DEBUG"
            ;;
        --skip-build)
            SKIP_BUILD=true
            ;;
        --help|-h)
            echo "Usage: $0 [--verbose] [--skip-build]"
            echo ""
            echo "Options:"
            echo "  --verbose, -v     Enable verbose output"
            echo "  --skip-build      Skip cargo build step"
            echo "  --help, -h        Show this help"
            exit 0
            ;;
    esac
done

# =============================================================================
# Logging functions (fallback if lib not loaded)
# =============================================================================

if ! declare -F log_info >/dev/null; then
    log_info() {
        echo "[INFO] $(date -Iseconds) $*"
    }
fi

if ! declare -F log_debug >/dev/null; then
    log_debug() {
        [[ "$VERBOSE" == "true" ]] && echo "[DEBUG] $(date -Iseconds) $*"
        return 0
    }
fi

if ! declare -F log_error >/dev/null; then
    log_error() {
        echo "[ERROR] $(date -Iseconds) $*" >&2
    }
fi

if ! declare -F log_success >/dev/null; then
    log_success() {
        echo "[OK] $*"
    }
fi

if ! declare -F log_fail >/dev/null; then
    log_fail() {
        echo "[FAIL] $*" >&2
    }
fi

jsonl_log() {
    local line="$1"
    [[ -n "$LOG_JSONL" ]] && mkdir -p "$E2E_RESULTS_DIR" && printf '%s\n' "$line" >> "$LOG_JSONL"
}

json_array() {
    python3 - "$@" <<'PY'
import json, sys
print(json.dumps(sys.argv[1:]))
PY
}

cmd_exists() {
    command -v "$1" >/dev/null 2>&1
}

bool_cmd() {
    if cmd_exists "$1"; then
        echo true
    else
        echo false
    fi
}

compute_checksum() {
    local file="$1"
    if cmd_exists sha256sum && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
    else
        echo "unavailable"
    fi
}

detect_capabilities() {
    local term_val="${TERM:-}"
    local colorterm="${COLORTERM:-}"
    if [[ -n "$colorterm" ]]; then
        echo "truecolor"
    elif [[ "$term_val" == *"256color"* ]]; then
        echo "256color"
    else
        echo "basic"
    fi
}

port_is_free() {
    local port="$1"
    python3 - "$port" <<'PY'
import socket, sys
port = int(sys.argv[1])
s = socket.socket()
try:
    s.bind(("127.0.0.1", port))
    s.close()
    sys.exit(0)
except OSError:
    sys.exit(1)
PY
}

pick_free_port() {
    python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
port = s.getsockname()[1]
s.close()
print(port)
PY
}

resolve_receiver_port() {
    if port_is_free "$RECEIVER_PORT"; then
        return 0
    fi
    local fallback
    fallback="$(pick_free_port)"
    log_info "Receiver port $RECEIVER_PORT is in use; using $fallback"
    RECEIVER_PORT="$fallback"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"receiver_port\",\"status\":\"fallback\",\"receiver_port\":$RECEIVER_PORT}"
}

# =============================================================================
# Cleanup on exit
# =============================================================================

RECEIVER_PID=""
cleanup() {
    if [[ -n "$RECEIVER_PID" ]]; then
        log_debug "Stopping OTEL receiver (PID=$RECEIVER_PID)"
        kill "$RECEIVER_PID" 2>/dev/null || true
        wait "$RECEIVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# =============================================================================
# Dependency check
# =============================================================================

check_dependencies() {
    if ! command -v python3 >/dev/null 2>&1; then
        log_error "python3 is required but not found"
        exit 3
    fi
    if [[ -z "${E2E_PYTHON:-}" ]]; then
        log_error "E2E_PYTHON is not set (python3/python not found)"
        exit 3
    fi
    if ! command -v curl >/dev/null 2>&1; then
        log_error "curl is required for receiver health check"
        exit 3
    fi
    if ! declare -F pty_run >/dev/null; then
        log_error "pty_run is required (tests/e2e/lib/pty.sh not loaded)"
        exit 3
    fi

    # Check if telemetry feature is available
    if ! grep -q 'telemetry' "$PROJECT_ROOT/crates/ftui-runtime/Cargo.toml" 2>/dev/null; then
        log_error "telemetry feature not found in ftui-runtime"
        exit 3
    fi
}

# =============================================================================
# OTEL HTTP Receiver (minimal Python server)
# =============================================================================

start_receiver() {
    log_info "Starting OTEL HTTP receiver on port $RECEIVER_PORT..."

    # Create a minimal Python HTTP server that accepts OTLP and logs spans
    cat > "$E2E_RESULTS_DIR/otel_receiver.py" <<'PYTHON'
#!/usr/bin/env python3
"""Minimal OTEL HTTP receiver for testing."""
import http.server
import json
import sys
import gzip
import re

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 14318
SPANS_FILE = sys.argv[2] if len(sys.argv) > 2 else "/tmp/captured_spans.jsonl"

class OTLPHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        # Suppress default logging
        pass

    def do_POST(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length)
        encoding = (self.headers.get('Content-Encoding', '') or '').lower()

        # Accept the request
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{}')

        # Try to extract span names from protobuf (simplified)
        # OTLP uses protobuf, but span names are stored as UTF-8 strings.
        try:
            spans = []
            raw = body
            if encoding == 'gzip' or (len(body) >= 2 and body[0:2] == b'\x1f\x8b'):
                try:
                    raw = gzip.decompress(body)
                except Exception as e:
                    print(f"Gzip decompress failed: {e}", file=sys.stderr)
                    raw = body
            print(f"Received {len(raw)} bytes on {self.path} encoding={encoding or 'none'}", file=sys.stderr)

            span_names = re.findall(rb'ftui\.[a-z_.]+', raw)

            for name_bytes in span_names:
                name = name_bytes.decode('utf-8', errors='ignore')
                span_record = {"span_name": name, "timestamp": __import__('datetime').datetime.now().isoformat()}
                spans.append(span_record)
                with open(SPANS_FILE, 'a') as f:
                    f.write(json.dumps(span_record) + '\n')
                print(f"Captured span: {name}", file=sys.stderr)
        except Exception as e:
            print(f"Parse error: {e}", file=sys.stderr)

    def do_GET(self):
        # Health check endpoint
        if self.path == '/health':
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
        else:
            self.send_response(404)
            self.end_headers()

class ReuseHTTPServer(http.server.HTTPServer):
    allow_reuse_address = True

if __name__ == '__main__':
    print(f"Starting OTEL receiver on port {PORT}", file=sys.stderr)
    print(f"Spans will be written to {SPANS_FILE}", file=sys.stderr)

    # Clear spans file
    open(SPANS_FILE, 'w').close()

    server = ReuseHTTPServer(('127.0.0.1', PORT), OTLPHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
PYTHON

    python3 "$E2E_RESULTS_DIR/otel_receiver.py" "$RECEIVER_PORT" "$SPANS_FILE" > "$RECEIVER_LOG" 2>&1 &
    RECEIVER_PID=$!

    # Wait for server to start
    sleep 1

    # Verify server is running
    if ! kill -0 "$RECEIVER_PID" 2>/dev/null; then
        log_error "Failed to start OTEL receiver"
        cat "$RECEIVER_LOG"
        exit 2
    fi

    # Health check
    if curl -s "http://127.0.0.1:$RECEIVER_PORT/health" | grep -q '"status":"ok"'; then
        log_debug "OTEL receiver is healthy"
    else
        log_error "OTEL receiver health check failed"
        exit 2
    fi

    CAP_OTEL_RECEIVER=true
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"receiver\",\"status\":\"started\",\"receiver_port\":$RECEIVER_PORT}"
    log_info "OTEL receiver started (PID=$RECEIVER_PID)"
}

# =============================================================================
# Setup
# =============================================================================

mkdir -p "$E2E_RESULTS_DIR" "$E2E_LOG_DIR"

# Timings come from e2e_monotonic_ms, not `date +%s%3N`: BSD date prints
# "...3N", which aborted the duration arithmetic on macOS.
START_TS="$(e2e_monotonic_ms)"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
RUN_ID="telemetry_${TIMESTAMP}_$$"
LOG_JSONL="$E2E_RESULTS_DIR/telemetry_${TIMESTAMP}.jsonl"
JSONL_FILE="$LOG_JSONL"
CAP_PYTHON="$(bool_cmd python3)"
CAP_CURL="$(bool_cmd curl)"
CAP_SHA256="$(bool_cmd sha256sum)"
CAP_PTY="$([[ -n "${E2E_PYTHON:-}" ]] && echo true || echo false)"
TERM_CAPS="$(detect_capabilities)"
CAPABILITIES_JSON="{\"python3\":$CAP_PYTHON,\"curl\":$CAP_CURL,\"sha256sum\":$CAP_SHA256,\"pty\":$CAP_PTY,\"term\":\"$TERM_CAPS\"}"

check_dependencies
resolve_receiver_port

# Environment log (JSONL format)
cat > "$E2E_RESULTS_DIR/env_${TIMESTAMP}.jsonl" <<EOF
{"event":"env","timestamp":"$(date -Iseconds)","user":"$(whoami)","hostname":"$(hostname)"}
{"event":"rust","rustc":"$(rustc --version 2>/dev/null || echo 'N/A')","cargo":"$(cargo --version 2>/dev/null || echo 'N/A')"}
{"event":"git","commit":"$(git rev-parse HEAD 2>/dev/null || echo 'N/A')","branch":"$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo 'N/A')"}
{"event":"config","receiver_port":$RECEIVER_PORT,"test_timeout":$TEST_TIMEOUT}
EOF

jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"env\",\"timestamp\":\"$(date -Iseconds)\",\"env\":{\"user\":\"$(whoami)\",\"hostname\":\"$(hostname)\",\"term\":\"${TERM:-}\",\"colorterm\":\"${COLORTERM:-}\",\"no_color\":\"${NO_COLOR:-}\",\"tmux\":\"${TMUX:-}\",\"zellij\":\"${ZELLIJ:-}\",\"kitty_window_id\":\"${KITTY_WINDOW_ID:-}\"},\"seed\":\"$SEED\",\"deterministic\":$([[ "$DETERMINISTIC" == "1" ]] && echo true || echo false),\"capabilities\":$CAPABILITIES_JSON}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"rust\",\"rustc\":\"$(rustc --version 2>/dev/null || echo 'N/A')\",\"cargo\":\"$(cargo --version 2>/dev/null || echo 'N/A')\"}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"git\",\"commit\":\"$(git rev-parse HEAD 2>/dev/null || echo 'N/A')\",\"branch\":\"$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo 'N/A')\"}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"config\",\"receiver_port\":$RECEIVER_PORT,\"test_timeout\":$TEST_TIMEOUT}"

log_info "Telemetry E2E Test (bd-1z02.9)"
log_info "Project root: $PROJECT_ROOT"
log_info "Output dir: $E2E_RESULTS_DIR"

# Deterministic mode controls
export FTUI_SEED="$SEED"
if [[ "$DETERMINISTIC" == "1" ]]; then
    export RUST_TEST_THREADS=1
    export TZ=UTC
    export LC_ALL=C
fi

# =============================================================================
# Build test binary
# =============================================================================

if [[ "$SKIP_BUILD" != "true" ]]; then
    log_info "Building ftui-harness with telemetry enabled..."
    BUILD_START="$(e2e_monotonic_ms)"

    if ! cargo build -p ftui-harness -F telemetry 2>"$E2E_RESULTS_DIR/build.log"; then
        log_error "Build failed! See $E2E_RESULTS_DIR/build.log"
        BUILD_END="$(e2e_monotonic_ms)"
        BUILD_MS=$((BUILD_END - BUILD_START))
        jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"build\",\"status\":\"failed\",\"timings\":{\"start_ms\":$BUILD_START,\"end_ms\":$BUILD_END,\"duration_ms\":$BUILD_MS}}"
        exit 2
    fi

    BUILD_END="$(e2e_monotonic_ms)"
    BUILD_MS=$((BUILD_END - BUILD_START))
    log_debug "Build completed in ${BUILD_MS}ms"
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"build\",\"status\":\"passed\",\"timings\":{\"start_ms\":$BUILD_START,\"end_ms\":$BUILD_END,\"duration_ms\":$BUILD_MS}}"
else
    log_info "Skipping build (--skip-build)"
    BUILD_START="$(e2e_monotonic_ms)"
    BUILD_END="$BUILD_START"
    BUILD_MS=0
    jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"build\",\"status\":\"skipped\",\"timings\":{\"start_ms\":$BUILD_START,\"end_ms\":$BUILD_END,\"duration_ms\":0}}"
fi

# =============================================================================
# Start receiver and run test
# =============================================================================

start_receiver

log_info "Running telemetry test..."
TEST_START="$(e2e_monotonic_ms)"

# Set OTEL environment variables
export OTEL_EXPORTER_OTLP_ENDPOINT="http://127.0.0.1:$RECEIVER_PORT"
export OTEL_TRACES_EXPORTER="otlp"
export OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf"
export OTEL_SERVICE_NAME="ftui-e2e-test"
export FTUI_OTEL_HTTP_ENDPOINT="http://127.0.0.1:$RECEIVER_PORT"
export FTUI_OTEL_SPAN_PROCESSOR="simple"
export FTUI_OTEL_PROCESSOR="simple"

log_debug "OTEL_EXPORTER_OTLP_ENDPOINT=$OTEL_EXPORTER_OTLP_ENDPOINT"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"otel_config\",\"endpoint\":\"$OTEL_EXPORTER_OTLP_ENDPOINT\",\"protocol\":\"$OTEL_EXPORTER_OTLP_PROTOCOL\",\"service\":\"$OTEL_SERVICE_NAME\",\"processor\":\"$FTUI_OTEL_SPAN_PROCESSOR\"}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"telemetry_config\",\"telemetry_enabled\":true,\"sdk_disabled\":false,\"endpoint\":\"$OTEL_EXPORTER_OTLP_ENDPOINT\",\"protocol\":\"$OTEL_EXPORTER_OTLP_PROTOCOL\",\"trace_context_source\":\"new\",\"processor\":\"$FTUI_OTEL_SPAN_PROCESSOR\"}"

TEST_OUTPUT_FILE="$E2E_RESULTS_DIR/telemetry_harness.pty"

# Run the harness with telemetry enabled and auto-exit
if PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_TIMEOUT="$TEST_TIMEOUT" \
    PTY_SEND="" \
    FTUI_HARNESS_EXIT_AFTER_MS=1200 \
    FTUI_HARNESS_SCREEN_MODE=inline \
    FTUI_HARNESS_LOG_LINES=5 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    FTUI_HARNESS_LOG_MARKUP=false \
    FTUI_HARNESS_LOG_KEYS=false \
    pty_run "$TEST_OUTPUT_FILE" cargo run -p ftui-harness -F telemetry > "$E2E_RESULTS_DIR/test_output.txt" 2>&1; then
    log_debug "Harness execution completed"
    TEST_STATUS="passed"
    TEST_EXIT=0
else
    log_debug "Harness exited with errors (checking spans anyway)"
    TEST_STATUS="warn"
    TEST_EXIT=1
fi

# Give time for spans to be exported
sleep 2

TEST_END="$(e2e_monotonic_ms)"
TEST_MS=$((TEST_END - TEST_START))
log_debug "Test completed in ${TEST_MS}ms"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"test\",\"status\":\"$TEST_STATUS\",\"exit_code\":$TEST_EXIT,\"timings\":{\"start_ms\":$TEST_START,\"end_ms\":$TEST_END,\"duration_ms\":$TEST_MS},\"output_file\":\"$TEST_OUTPUT_FILE\"}"

# =============================================================================
# Validate captured spans
# =============================================================================

log_info "Validating captured spans..."

MISSING_SPANS=()
FOUND_SPANS=()

if [[ -f "$SPANS_FILE" && -s "$SPANS_FILE" ]]; then
    log_debug "Captured spans file exists and is non-empty"

    for span in "${REQUIRED_SPANS[@]}"; do
        if grep -Eq "\"span_name\"[[:space:]]*:[[:space:]]*\"$span\"" "$SPANS_FILE"; then
            log_debug "Found required span: $span"
            FOUND_SPANS+=("$span")
        else
            log_debug "Missing required span: $span"
            MISSING_SPANS+=("$span")
        fi
    done
else
    log_debug "No spans captured - marking all required spans as missing"
    for span in "${REQUIRED_SPANS[@]}"; do
        MISSING_SPANS+=("$span")
    done
fi

SPAN_COUNT=0
SPAN_CHECKSUM="unavailable"
if [[ -f "$SPANS_FILE" ]]; then
    SPAN_COUNT=$(wc -l < "$SPANS_FILE" | tr -d ' ')
    SPAN_CHECKSUM="$(compute_checksum "$SPANS_FILE")"
fi
TEST_OUTPUT_CHECKSUM="$(compute_checksum "$TEST_OUTPUT_FILE")"
RECEIVER_CHECKSUM="$(compute_checksum "$RECEIVER_LOG")"

# =============================================================================
# Final results
# =============================================================================

END_TS="$(e2e_monotonic_ms)"
TOTAL_MS=$((END_TS - START_TS))

FOUND_JSON="$(json_array "${FOUND_SPANS[@]}")"
MISSING_JSON="$(json_array "${MISSING_SPANS[@]}")"

if [[ ${#MISSING_SPANS[@]} -eq 0 ]]; then
    FINAL_STATUS="pass"
else
    FINAL_STATUS="fail"
fi

if [[ "$FINAL_STATUS" == "fail" ]]; then
    OUTCOME_JSON='{"status":"fail","reason":"missing_spans"}'
else
    OUTCOME_JSON='{"status":"pass"}'
fi

# Write results JSONL (summary file + unified log)
cat > "$E2E_RESULTS_DIR/results_${TIMESTAMP}.jsonl" <<EOF
{"run_id":"$RUN_ID","case":"$CASE","event":"summary","status":"$FINAL_STATUS","total_ms":$TOTAL_MS,"build_ms":$BUILD_MS,"test_ms":$TEST_MS,"spans_found":${#FOUND_SPANS[@]},"spans_missing":${#MISSING_SPANS[@]},"spans_required":${#REQUIRED_SPANS[@]},"span_count":$SPAN_COUNT,"span_checksum":"$SPAN_CHECKSUM"}
{"run_id":"$RUN_ID","case":"$CASE","event":"found_spans","spans":$FOUND_JSON}
{"run_id":"$RUN_ID","case":"$CASE","event":"missing_spans","spans":$MISSING_JSON}
EOF

RUN_CAPABILITIES_JSON="{\"python3\":$CAP_PYTHON,\"curl\":$CAP_CURL,\"sha256sum\":$CAP_SHA256,\"pty\":$CAP_PTY,\"term\":\"$TERM_CAPS\",\"otel_receiver\":$CAP_OTEL_RECEIVER,\"receiver_port\":$RECEIVER_PORT,\"protocol\":\"$OTEL_EXPORTER_OTLP_PROTOCOL\"}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"spans\",\"found\":${#FOUND_SPANS[@]},\"missing\":${#MISSING_SPANS[@]},\"required\":${#REQUIRED_SPANS[@]},\"count\":$SPAN_COUNT,\"checksums\":{\"spans\":\"$SPAN_CHECKSUM\",\"test_output\":\"$TEST_OUTPUT_CHECKSUM\",\"receiver_log\":\"$RECEIVER_CHECKSUM\"}}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"found_spans\",\"spans\":$FOUND_JSON}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"missing_spans\",\"spans\":$MISSING_JSON}"
jsonl_log "{\"run_id\":\"$RUN_ID\",\"case\":\"$CASE\",\"event\":\"summary\",\"status\":\"$FINAL_STATUS\",\"timings\":{\"total_ms\":$TOTAL_MS,\"build_ms\":$BUILD_MS,\"test_ms\":$TEST_MS},\"checksums\":{\"spans\":\"$SPAN_CHECKSUM\",\"test_output\":\"$TEST_OUTPUT_CHECKSUM\",\"receiver_log\":\"$RECEIVER_CHECKSUM\"},\"capabilities\":$RUN_CAPABILITIES_JSON,\"outcome\":$OUTCOME_JSON}"

# Copy receiver log to results
if [[ -f "$RECEIVER_LOG" ]]; then
    cp "$RECEIVER_LOG" "$E2E_RESULTS_DIR/receiver_${TIMESTAMP}.log"
fi

log_info "================================================"
if [[ ${#MISSING_SPANS[@]} -eq 0 ]]; then
    log_success "Telemetry E2E Test PASSED"
    log_info "Found ${#FOUND_SPANS[@]} of ${#REQUIRED_SPANS[@]} required spans"
    EXIT_CODE=0
else
    log_fail "Telemetry E2E Test FAILED"
    log_error "Missing ${#MISSING_SPANS[@]} spans: ${MISSING_SPANS[*]}"
    EXIT_CODE=1
fi

log_info "Total time: ${TOTAL_MS}ms"
log_info "Results: $E2E_RESULTS_DIR"
log_info "================================================"

exit $EXIT_CODE
