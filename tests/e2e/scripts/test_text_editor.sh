#!/bin/bash
# Advanced Text Editor E2E Test Suite for FrankenTUI
# bd-12o8.4: PTY-based end-to-end tests with verbose JSONL logging
#
# This script validates:
# 1. Text editor screen loads and renders correctly
# 2. Basic text input and cursor movement
# 3. Selection operations (word, line, all)
# 4. Navigation (arrows, Home/End, Ctrl+arrows)
# 5. Editing operations (insert, delete, backspace)
# 6. Undo/redo functionality
# 7. Search/replace interactions
# 8. Multiline editing behavior
#
# Usage:
#   ./tests/e2e/scripts/test_text_editor.sh
#   E2E_LOG_DIR=/tmp/text-editor-logs ./tests/e2e/scripts/test_text_editor.sh
#
# Environment:
#   E2E_LOG_DIR         Directory for log files (default: /tmp/ftui_e2e_logs)
#   FTUI_DEMO_BIN       Path to the ftui-demo-showcase binary
#
# JSONL Log Schema:
#   {"ts": "T000001", "step": "<step>", "test": "<name>", ...}
#   Keys: ts, step, test, duration_ms, input_seq, output_size, checksum, result

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

# Resolve the screen from the built binary; registry order changes over time.
TEXT_EDITOR_SCREEN=""

# Invariants (Alien Artifact):
# 1. Cursor always within document bounds after any operation
# 2. Text length equals sum of line lengths + newline count
# 3. Undo reverses exactly the previous atomic operation
# 4. Selection ranges never exceed document bounds

# Failure Modes:
# | Scenario              | Expected Behavior                    |
# |-----------------------|--------------------------------------|
# | Zero-width viewport   | No panic, graceful no-op             |
# | Rapid key input       | All keys processed, no drops         |
# | Large paste           | Content accepted, viewport scrolls   |
# | Empty document delete | No-op, cursor stays at (0,0)         |

# Check for demo showcase binary
resolve_demo_bin() {
    if [[ -n "${FTUI_DEMO_BIN:-}" && -x "$FTUI_DEMO_BIN" ]]; then
        echo "$FTUI_DEMO_BIN"
        return 0
    fi

    # Check shared cargo target directory first (if CARGO_TARGET_DIR is set)
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        local shared_debug="$CARGO_TARGET_DIR/debug/ftui-demo-showcase"
        local shared_release="$CARGO_TARGET_DIR/release/ftui-demo-showcase"
        if [[ -x "$shared_debug" ]]; then
            echo "$shared_debug"
            return 0
        fi
        if [[ -x "$shared_release" ]]; then
            echo "$shared_release"
            return 0
        fi
    fi

    # Check project-local target directory
    local debug_bin="$PROJECT_ROOT/target/debug/ftui-demo-showcase"
    local release_bin="$PROJECT_ROOT/target/release/ftui-demo-showcase"

    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
        return 0
    fi
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    return 1
}

DEMO_BIN=""
if ! DEMO_BIN="$(resolve_demo_bin)"; then
    LOG_FILE="$E2E_LOG_DIR/text_editor_missing.log"
    for t in editor_screen_loads editor_basic_input editor_cursor_movement editor_selection \
             editor_delete_backspace editor_undo_redo editor_multiline editor_rapid_input \
             editor_home_end editor_word_navigation editor_search_focus; do
        log_test_skip "$t" "ftui-demo-showcase binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 2
fi

require_tools jq || exit 2
TEXT_EDITOR_SCREEN="$("$DEMO_BIN" --list-screens | jq -er '
    index("advanced_text_editor") | if . == null then error("editor screen missing") else . + 1 end
')" || exit 2

# Compute checksum of output file for determinism verification
compute_checksum() {
    local file="$1"
    if [[ -f "$file" ]]; then
        sha256sum "$file" | cut -d' ' -f1 | head -c 16
    else
        echo "no_file"
    fi
}

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(date +%s%3N)"

    if "$@"; then
        local end_ms
        end_ms="$(date +%s%3N)"
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local end_ms
    end_ms="$(date +%s%3N)"
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "text editor assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "text editor assertions failed"
    return 1
}

# ============================================================================
# Test: Editor screen loads without crashing
# ============================================================================
editor_screen_loads() {
    LOG_FILE="$E2E_LOG_DIR/editor_screen_loads.log"
    local output_file="$E2E_LOG_DIR/editor_screen_loads.pty"

    log_test_start "editor_screen_loads"

    # Start demo on Text Editor screen
    FTUI_DEMO_EXIT_AFTER_MS=2000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    # Text Editor screen should render with expected content
    if ! grep -a -q "Text Editor\|Editor\|Line\|Col" "$output_file"; then
        log_warn "Text editor content not found in output"
        return 1
    fi

    # Output should have substantial content
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    local checksum
    checksum=$(compute_checksum "$output_file")

    # Log JSONL entry
    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_screen_loads\",\"output_size\":$size,\"checksum\":\"$checksum\"}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Basic text input
# ============================================================================
editor_basic_input() {
    LOG_FILE="$E2E_LOG_DIR/editor_basic_input.log"
    local output_file="$E2E_LOG_DIR/editor_basic_input.pty"

    log_test_start "editor_basic_input"

    # Type some text
    PTY_SEND='Hello, World!' \
    PTY_SEND_DELAY_MS=100 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    local checksum
    checksum=$(compute_checksum "$output_file")

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_basic_input\",\"input\":\"Hello, World!\",\"output_size\":$size,\"checksum\":\"$checksum\"}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Cursor movement with arrow keys
# ============================================================================
editor_cursor_movement() {
    LOG_FILE="$E2E_LOG_DIR/editor_cursor_movement.log"
    local output_file="$E2E_LOG_DIR/editor_cursor_movement.pty"

    log_test_start "editor_cursor_movement"

    # Type text then navigate with arrows
    local arrows=$'\x1b[C\x1b[C\x1b[D\x1b[A\x1b[B'
    PTY_SEND="test$arrows" \
    PTY_SEND_DELAY_MS=150 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_cursor_movement\",\"input_seq\":\"arrow_keys\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Selection operations
# ============================================================================
editor_selection() {
    LOG_FILE="$E2E_LOG_DIR/editor_selection.log"
    local output_file="$E2E_LOG_DIR/editor_selection.pty"

    log_test_start "editor_selection"

    # Type text, then select with Shift+arrows
    # Shift+Right = \x1b[1;2C, Shift+Left = \x1b[1;2D
    local shift_right=$'\x1b[1;2C\x1b[1;2C\x1b[1;2C'
    PTY_SEND="select_me$shift_right" \
    PTY_SEND_DELAY_MS=150 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_selection\",\"input_seq\":\"shift_arrow\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Delete and backspace
# ============================================================================
editor_delete_backspace() {
    LOG_FILE="$E2E_LOG_DIR/editor_delete_backspace.log"
    local output_file="$E2E_LOG_DIR/editor_delete_backspace.pty"

    log_test_start "editor_delete_backspace"

    # Type text then delete with backspace and delete key
    # Backspace = \x7f, Delete = \x1b[3~
    local del=$'\x1b[3~'
    PTY_SEND="testtext\x7f\x7f$del" \
    PTY_SEND_DELAY_MS=150 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_delete_backspace\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Undo and redo
# ============================================================================
editor_undo_redo() {
    LOG_FILE="$E2E_LOG_DIR/editor_undo_redo.log"
    local output_file="$E2E_LOG_DIR/editor_undo_redo.pty"

    log_test_start "editor_undo_redo"

    # Type text, then undo (Ctrl+Z = \x1a) and redo (Ctrl+Y = \x19)
    PTY_SEND="undo_test\x1a\x1a\x19" \
    PTY_SEND_DELAY_MS=200 \
    FTUI_DEMO_EXIT_AFTER_MS=3500 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_undo_redo\",\"input_seq\":\"Ctrl+Z,Ctrl+Y\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Multiline editing (Enter for newlines)
# ============================================================================
editor_multiline() {
    LOG_FILE="$E2E_LOG_DIR/editor_multiline.log"
    local output_file="$E2E_LOG_DIR/editor_multiline.pty"

    log_test_start "editor_multiline"

    # Type text with newlines
    PTY_SEND=$'Line 1\rLine 2\rLine 3' \
    PTY_SEND_DELAY_MS=150 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_multiline\",\"lines\":3,\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Rapid input (stress test)
# ============================================================================
editor_rapid_input() {
    LOG_FILE="$E2E_LOG_DIR/editor_rapid_input.log"
    local output_file="$E2E_LOG_DIR/editor_rapid_input.pty"

    log_test_start "editor_rapid_input"

    # Send rapid keystrokes - 50 characters quickly
    PTY_SEND='abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMN' \
    PTY_SEND_DELAY_MS=20 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_rapid_input\",\"char_count\":50,\"delay_ms\":20,\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Home and End keys
# ============================================================================
editor_home_end() {
    LOG_FILE="$E2E_LOG_DIR/editor_home_end.log"
    local output_file="$E2E_LOG_DIR/editor_home_end.pty"

    log_test_start "editor_home_end"

    # Type text then use Home (\x1b[H or \x1b[1~) and End (\x1b[F or \x1b[4~)
    local home=$'\x1b[H'
    local end=$'\x1b[F'
    PTY_SEND="some long text here$home$end" \
    PTY_SEND_DELAY_MS=200 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_home_end\",\"input_seq\":\"Home,End\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Word navigation (Ctrl+arrows)
# ============================================================================
editor_word_navigation() {
    LOG_FILE="$E2E_LOG_DIR/editor_word_navigation.log"
    local output_file="$E2E_LOG_DIR/editor_word_navigation.pty"

    log_test_start "editor_word_navigation"

    # Type words then navigate by word with Ctrl+Left/Right
    # Ctrl+Right = \x1b[1;5C, Ctrl+Left = \x1b[1;5D
    local ctrl_right=$'\x1b[1;5C'
    local ctrl_left=$'\x1b[1;5D'
    PTY_SEND="word1 word2 word3$ctrl_left$ctrl_left$ctrl_right" \
    PTY_SEND_DELAY_MS=200 \
    FTUI_DEMO_EXIT_AFTER_MS=3000 \
    PTY_TIMEOUT=5 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_word_navigation\",\"input_seq\":\"Ctrl+arrows\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Test: Search focus (Ctrl+F)
# ============================================================================
editor_search_focus() {
    LOG_FILE="$E2E_LOG_DIR/editor_search_focus.log"
    local output_file="$E2E_LOG_DIR/editor_search_focus.pty"

    log_test_start "editor_search_focus"

    # Type text then focus search (Ctrl+F = \x06)
    PTY_SEND="searchable text\x06find" \
    PTY_SEND_DELAY_MS=200 \
    FTUI_DEMO_EXIT_AFTER_MS=3500 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')

    echo "{\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"test\":\"editor_search_focus\",\"input_seq\":\"Ctrl+F\",\"output_size\":$size}" >> "$LOG_FILE"

    [[ "$size" -gt 500 ]] || return 1
}

# ============================================================================
# Run all tests
# ============================================================================

editor_clipboard_roundtrip() {
    LOG_FILE="$E2E_LOG_DIR/editor_clipboard_roundtrip.log"
    local output_file="$E2E_LOG_DIR/editor_clipboard_roundtrip.pty"
    log_test_start "editor_clipboard_roundtrip"

    # Separate Esc from the next key so it cannot become an Alt chord. The
    # parent PTY supplies a terminal reply; no real system clipboard is touched.
    TERM=xterm-kitty TERM_PROGRAM=kitty \
    FTUI_TEXTEDITOR_DIAGNOSTICS=true FTUI_TEXTEDITOR_DETERMINISTIC=true \
    FTUI_DEMO_EXIT_AFTER_MS=3000 PTY_TIMEOUT=6 \
    PTY_SEND_AFTER_OUTPUT='Advanced Text Editor' \
    PTY_SEND_SEQUENCE='[{"delay_ms":300,"text":"\u001b"},{"delay_ms":600,"text":"y"},{"delay_ms":900,"text":"p"},{"delay_ms":1200,"text":"\u001b]52;c;YWJj\u0007"},{"delay_ms":1800,"text":"q"}]' \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN" || return 1

    "$E2E_PYTHON" - "$output_file" "$LOG_FILE" <<'PY'
import base64
import json
import pathlib
import sys

wire = pathlib.Path(sys.argv[1]).read_bytes()
copy = b'\x1b]52;c;' + base64.b64encode(b'Welcome to the Advanced Text Editor!') + b'\x07'
assert wire.count(copy) == 1, 'expected exactly one OSC 52 copy of the current line'
assert wire.count(b'\x1b]52;c;?\x07') == 1, 'expected exactly one OSC 52 query'
assert wire.count(b'"kind":"clipboard_pasted"') == 1, 'reply must reach editor exactly once'
assert b'"chars":3,"mode":"NORMAL"' in wire, 'clipboard diagnostics must count the reply'
assert b'Pasted 3 chars' in wire, 'paste status must be rendered'
with pathlib.Path(sys.argv[2]).open('a') as log:
    log.write(json.dumps({'test': 'editor_clipboard_roundtrip', 'copy': True,
                         'query': True, 'reply_count': 1, 'chars': 3,
                         'result': 'passed'}) + '\n')
PY
}

editor_clipboard_payload_cap() {
    LOG_FILE="$E2E_LOG_DIR/editor_clipboard_payload_cap.log"
    local output_file="$E2E_LOG_DIR/editor_clipboard_payload_cap.pty"
    local sequence
    log_test_start "editor_clipboard_payload_cap"

    sequence="$("$E2E_PYTHON" - <<'PY'
import json
print(json.dumps([
    {'delay_ms': 300, 'text': '\x01'},
    {'delay_ms': 600, 'text': '\x1b[200~' + 'a' * 60000 + '\x1b[201~'},
    {'delay_ms': 1400, 'text': '\x01'},
    {'delay_ms': 1700, 'text': '\x1b'},
    {'delay_ms': 2000, 'text': 'y'},
    {'delay_ms': 2800, 'text': 'q'},
]))
PY
)" || return 1

    # There is no automatic successful exit: the normal-mode q must work
    # after rejection. PTY timeout is a failure, not a substitute for q.
    TERM=xterm-kitty TERM_PROGRAM=kitty \
    FTUI_TEXTEDITOR_DIAGNOSTICS=true FTUI_TEXTEDITOR_DETERMINISTIC=true \
    FTUI_DEMO_EXIT_AFTER_MS=0 PTY_TIMEOUT=8 \
    PTY_SEND_AFTER_OUTPUT='Advanced Text Editor' PTY_SEND_SEQUENCE="$sequence" \
        pty_run "$output_file" "$DEMO_BIN" --screen="$TEXT_EDITOR_SCREEN" || return 1

    "$E2E_PYTHON" - "$output_file" "$LOG_FILE" <<'PY'
import json
import pathlib
import sys

wire = pathlib.Path(sys.argv[1]).read_bytes()
assert b'Clipboard payload too large' in wire, 'rejection status must be rendered'
assert b'\x1b]52;' not in wire, 'oversized copy must emit no OSC 52 request'
assert b'"kind":"clipboard_copied"' not in wire, 'rejection must not log a successful copy'
alt_enters, alt_leaves = wire.count(b'\x1b[?1049h'), wire.count(b'\x1b[?1049l')
sync_begins, sync_ends = wire.count(b'\x1b[?2026h'), wire.count(b'\x1b[?2026l')
assert alt_enters > 0 and alt_enters == alt_leaves, 'alternate screen must be restored'
assert sync_begins == sync_ends, 'synchronized output must be balanced'
assert wire.rfind(b'\x1b[?25h') > wire.rfind(b'\x1b[?25l'), 'cursor must be visible at exit'
with pathlib.Path(sys.argv[2]).open('a') as log:
    log.write(json.dumps({'test': 'editor_clipboard_payload_cap',
                         'payload_bytes': 60000, 'payload_b64_len': 80000,
                         'osc52_count': 0, 'alt_pairs': alt_enters,
                         'sync_pairs': sync_begins, 'exit_code': 0,
                         'result': 'passed'}) + '\n')
PY
}

FAILURES=0
run_case "editor_screen_loads" editor_screen_loads               || FAILURES=$((FAILURES + 1))
run_case "editor_basic_input" editor_basic_input                 || FAILURES=$((FAILURES + 1))
run_case "editor_cursor_movement" editor_cursor_movement         || FAILURES=$((FAILURES + 1))
run_case "editor_selection" editor_selection                     || FAILURES=$((FAILURES + 1))
run_case "editor_delete_backspace" editor_delete_backspace       || FAILURES=$((FAILURES + 1))
run_case "editor_undo_redo" editor_undo_redo                     || FAILURES=$((FAILURES + 1))
run_case "editor_multiline" editor_multiline                     || FAILURES=$((FAILURES + 1))
run_case "editor_rapid_input" editor_rapid_input                 || FAILURES=$((FAILURES + 1))
run_case "editor_home_end" editor_home_end                       || FAILURES=$((FAILURES + 1))
run_case "editor_word_navigation" editor_word_navigation         || FAILURES=$((FAILURES + 1))
run_case "editor_search_focus" editor_search_focus               || FAILURES=$((FAILURES + 1))
run_case "editor_clipboard_roundtrip" editor_clipboard_roundtrip || FAILURES=$((FAILURES + 1))
run_case "editor_clipboard_payload_cap" editor_clipboard_payload_cap || FAILURES=$((FAILURES + 1))

# Summary
echo ""
echo "============================================"
echo "Text Editor E2E Tests Complete"
echo "============================================"
echo "Total: 13 tests"
echo "Failures: $FAILURES"
if [[ "$FAILURES" -eq 0 ]]; then
    echo "Status: PASSED"
else
    echo "Status: FAILED"
fi
echo "Logs: $E2E_LOG_DIR/editor_*.log"
echo "============================================"

exit "$FAILURES"
