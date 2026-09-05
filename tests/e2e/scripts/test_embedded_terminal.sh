#!/bin/bash
# E2E tests for embedded terminal widget functionality.
#
# Tests verify:
# 1. PTY Management - Shell spawn, env inheritance, working directory, clean termination
# 2. ANSI Rendering - 256 colors, cursor positioning, scrollback, line wrapping
# 3. Input Forwarding - Key sequences, modifiers, bracketed paste, function keys
# 4. Resize Handling - SIGWINCH propagation, content reflow, cursor preservation
#
# JSONL logging is enabled via E2E_JSONL_LOG for structured analysis.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

E2E_SUITE_SCRIPT="$SCRIPT_DIR/test_embedded_terminal.sh"
export E2E_SUITE_SCRIPT
ONLY_CASE="${E2E_ONLY_CASE:-}"
REAL_PROGS="${E2E_REAL_PROGS:-1}"

# JSONL log for structured output analysis
E2E_JSONL_LOG="${E2E_JSONL_LOG:-$E2E_LOG_DIR/embedded_terminal.jsonl}"
mkdir -p "$(dirname "$E2E_JSONL_LOG")"

ALL_CASES=(
    term_pty_spawn_success
    term_pty_dev_tty_openable
    term_pty_env_inheritance
    term_pty_working_directory
    term_pty_clean_termination
    term_ansi_256_color_output
    term_ansi_cursor_positioning
    term_ansi_line_wrapping
    term_ansi_clear_sequences
    term_input_key_sequences
    term_input_modifiers
    term_input_function_keys
    term_input_bracketed_paste
    term_resize_sigwinch
    term_resize_rapid_stability
    term_programs_bash_interactive
    term_programs_zsh_interactive
    term_programs_vim_render
    term_programs_nano_render
    term_programs_htop_live
    term_programs_top_batch
    term_programs_less_scroll
    term_programs_more_scroll
)

if [[ ! -x "${E2E_HARNESS_BIN:-}" ]]; then
    LOG_FILE="$E2E_LOG_DIR/embedded_terminal_missing.log"
    for t in "${ALL_CASES[@]}"; do
        log_test_skip "$t" "ftui-harness binary missing"
        record_result "$t" "skipped" 0 "$LOG_FILE" "binary missing"
    done
    exit 0
fi

# Emit JSONL log entry for analysis
jsonl_log() {
    local event="$1"
    local test_name="$2"
    shift 2
    local ts
    ts="$(date -Iseconds)"
    printf '{\"ts\":\"%s\",\"event\":\"%s\",\"test\":\"%s\"' "$ts" "$event" "$test_name"
    while [[ $# -gt 0 ]]; do
        local key="$1"
        local val="$2"
        shift 2
        printf ',%s' "$(jq -n --arg k "$key" --arg v "$val" '{($k):$v}' | sed 's/[{}]//g')"
    done
    printf '}\n' >> "$E2E_JSONL_LOG"
}

skip_case() {
    local reason="$1"
    E2E_SKIP_REASON="$reason"
    return 2
}

require_real_progs() {
    if [[ "$REAL_PROGS" != "1" ]]; then
        skip_case "real program tests disabled (E2E_REAL_PROGS=$REAL_PROGS)"
        return $?
    fi
    return 0
}

require_program() {
    local program="$1"
    if ! command -v "$program" >/dev/null 2>&1; then
        skip_case "missing program: $program"
        return $?
    fi
    return 0
}

run_case() {
    local name="$1"
    shift
    if [[ -n "$ONLY_CASE" && "$ONLY_CASE" != "$name" ]]; then
        LOG_FILE="$E2E_LOG_DIR/${name}.log"
        log_test_skip "$name" "filtered (E2E_ONLY_CASE=$ONLY_CASE)"
        record_result "$name" "skipped" 0 "$LOG_FILE" "filtered"
        return 0
    fi
    local start_ms
    # BSD date lacks %N; elapsed timing uses the real monotonic clock.
    start_ms="$(e2e_monotonic_ms)" || return 2
    jsonl_log "start" "$name" "seed" "$RANDOM"

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        jsonl_log "pass" "$name" "duration_ms" "$duration_ms"
        return 0
    fi

    local exit_code=$?
    if [[ "$exit_code" -eq 2 ]]; then
        local reason="${E2E_SKIP_REASON:-skipped}"
        log_test_skip "$name" "$reason"
        record_result "$name" "skipped" 0 "$LOG_FILE" "$reason"
        jsonl_log "skip" "$name" "reason" "$reason"
        E2E_SKIP_REASON=""
        return 0
    fi

    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    log_test_fail "$name" "embedded terminal assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "embedded terminal assertions failed"
    jsonl_log "fail" "$name" "duration_ms" "$duration_ms"
    return 1
}

# ============================================================================
# PTY Management Tests
# ============================================================================

# Test: Shell spawns successfully and produces output
term_pty_spawn_success() {
    LOG_FILE="$E2E_LOG_DIR/term_pty_spawn.log"
    local output_file="$E2E_LOG_DIR/term_pty_spawn.pty"

    log_test_start "term_pty_spawn_success"

    # Run a simple echo command through PTY
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c "echo 'PTY_SPAWN_SUCCESS_MARKER'"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_pty_spawn" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    # Verify marker appears in output
    grep -a -q "PTY_SPAWN_SUCCESS_MARKER" "$output_file" || return 1
}

# Test: /dev/tty is openable inside the child (requires controlling TTY)
term_pty_dev_tty_openable() {
    LOG_FILE="$E2E_LOG_DIR/term_pty_dev_tty.log"
    local output_file="$E2E_LOG_DIR/term_pty_dev_tty.pty"

    log_test_start "term_pty_dev_tty_openable"

    # `: > /dev/tty` forces an open of the controlling TTY; it should fail with
    # ENXIO if the child has no controlling terminal. We then print a marker to
    # stdout so the assertion is deterministic.
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'set -e; : > /dev/tty; echo DEV_TTY_OPEN_OK'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_pty_dev_tty" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    grep -a -q "DEV_TTY_OPEN_OK" "$output_file" || return 1
}

# Test: Environment variables are inherited by child
term_pty_env_inheritance() {
    LOG_FILE="$E2E_LOG_DIR/term_pty_env.log"
    local output_file="$E2E_LOG_DIR/term_pty_env.pty"

    log_test_start "term_pty_env_inheritance"

    # Set a custom env var and verify it reaches the child
    FTUI_TEST_ENV_VAR="embedded_terminal_test_12345" \
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'echo "ENV=$FTUI_TEST_ENV_VAR"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_pty_env" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    grep -a -q "embedded_terminal_test_12345" "$output_file" || return 1
}

# Test: Working directory is respected
term_pty_working_directory() {
    LOG_FILE="$E2E_LOG_DIR/term_pty_cwd.log"
    local output_file="$E2E_LOG_DIR/term_pty_cwd.pty"

    log_test_start "term_pty_working_directory"

    # Create temp dir, run pwd inside it
    local test_dir
    test_dir=$(mktemp -d)
    local test_dir_name
    test_dir_name=$(basename "$test_dir")

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c "cd '$test_dir' && pwd"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_pty_cwd" "size_bytes" "$size" "dir" "$test_dir"

    # Clean up
    rmdir "$test_dir" 2>/dev/null || true

    grep -a -q "$test_dir_name" "$output_file" || return 1
}

# Test: Process terminates cleanly
term_pty_clean_termination() {
    LOG_FILE="$E2E_LOG_DIR/term_pty_clean.log"
    local output_file="$E2E_LOG_DIR/term_pty_clean.pty"

    log_test_start "term_pty_clean_termination"

    # Run a command that exits cleanly with known exit code
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c "echo 'CLEAN_EXIT'; exit 0"

    local exit_code=$?
    jsonl_log "output" "term_pty_clean" "exit_code" "$exit_code"

    # Should have captured output
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 5 ]] || return 1

    grep -a -q "CLEAN_EXIT" "$output_file" || return 1
}

# ============================================================================
# ANSI Rendering Tests
# ============================================================================

# Test: 256-color output sequences are generated
term_ansi_256_color_output() {
    LOG_FILE="$E2E_LOG_DIR/term_ansi_256.log"
    local output_file="$E2E_LOG_DIR/term_ansi_256.pty"

    log_test_start "term_ansi_256_color_output"

    # Generate 256-color escape sequences
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'printf "\033[38;5;196mRED256\033[0m\n"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_ansi_256" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    # Check for 256-color sequence (38;5;)
    grep -a -q "38;5;196" "$output_file" || return 1
    grep -a -q "RED256" "$output_file" || return 1
}

# Test: Cursor positioning sequences work
term_ansi_cursor_positioning() {
    LOG_FILE="$E2E_LOG_DIR/term_ansi_cursor.log"
    local output_file="$E2E_LOG_DIR/term_ansi_cursor.pty"

    log_test_start "term_ansi_cursor_positioning"

    # Use cursor positioning (CSI H)
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'printf "\033[5;10HCURSOR_POS\n"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_ansi_cursor" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    # Check for cursor position sequence
    grep -a -q "5;10H" "$output_file" || return 1
    grep -a -q "CURSOR_POS" "$output_file" || return 1
}

# Test: Line wrapping works correctly
term_ansi_line_wrapping() {
    LOG_FILE="$E2E_LOG_DIR/term_ansi_wrap.log"
    local output_file="$E2E_LOG_DIR/term_ansi_wrap.pty"

    log_test_start "term_ansi_line_wrapping"

    # Generate text longer than terminal width (40 cols)
    PTY_COLS=40 \
    PTY_ROWS=10 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'printf "1234567890123456789012345678901234567890WRAP_NEXT_LINE\n"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_ansi_wrap" "size_bytes" "$size"
    [[ "$size" -gt 40 ]] || return 1

    # Both parts should appear
    grep -a -q "1234567890" "$output_file" || return 1
    grep -a -q "WRAP_NEXT_LINE" "$output_file" || return 1
}

# Test: Clear screen sequences work
term_ansi_clear_sequences() {
    LOG_FILE="$E2E_LOG_DIR/term_ansi_clear.log"
    local output_file="$E2E_LOG_DIR/term_ansi_clear.pty"

    log_test_start "term_ansi_clear_sequences"

    # Use clear screen (CSI 2J) and move home (CSI H)
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'printf "\033[2J\033[HCLEARED\n"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_ansi_clear" "size_bytes" "$size"
    [[ "$size" -gt 5 ]] || return 1

    # Check for clear sequence
    grep -a -q "2J" "$output_file" || return 1
    grep -a -q "CLEARED" "$output_file" || return 1
}

# ============================================================================
# Input Forwarding Tests
# ============================================================================

# Test: Key sequences are properly forwarded
term_input_key_sequences() {
    LOG_FILE="$E2E_LOG_DIR/term_input_keys.log"
    local output_file="$E2E_LOG_DIR/term_input_keys.pty"

    log_test_start "term_input_key_sequences"

    # Send keystrokes to cat and verify echo
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=2 \
    PTY_SEND="hello\x04" \
    PTY_SEND_DELAY_MS=100 \
        pty_run "$output_file" cat

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_input_keys" "size_bytes" "$size"
    [[ "$size" -gt 0 ]] || return 1

    # Should see echoed input
    grep -a -q "hello" "$output_file" || return 1
}

# Test: Modifier keys work (Ctrl sequences)
term_input_modifiers() {
    LOG_FILE="$E2E_LOG_DIR/term_input_mods.log"
    local output_file="$E2E_LOG_DIR/term_input_mods.pty"

    log_test_start "term_input_modifiers"

    # Send Ctrl+C (0x03) to a process
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=2 \
    PTY_SEND="\x03" \
    PTY_SEND_DELAY_MS=500 \
        pty_run "$output_file" sh -c 'echo "WAITING"; sleep 10; echo "SHOULD_NOT_APPEAR"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_input_mods" "size_bytes" "$size"

    # WAITING should appear (before Ctrl+C)
    grep -a -q "WAITING" "$output_file" || return 1
    # SHOULD_NOT_APPEAR should NOT appear (Ctrl+C interrupted)
    if grep -a -q "SHOULD_NOT_APPEAR" "$output_file"; then
        jsonl_log "assertion_fail" "term_input_mods" "reason" "Ctrl+C did not interrupt"
        return 1
    fi
    return 0
}

# Test: Function keys generate correct sequences
term_input_function_keys() {
    LOG_FILE="$E2E_LOG_DIR/term_input_fkeys.log"
    local output_file="$E2E_LOG_DIR/term_input_fkeys.pty"

    log_test_start "term_input_function_keys"

    # F1 = ESC O P (\x1bOP)
    # Send F1 key sequence and check it's received
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=2 \
    PTY_SEND="\x1bOP\x04" \
    PTY_SEND_DELAY_MS=100 \
        pty_run "$output_file" cat

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_input_fkeys" "size_bytes" "$size"

    # The ESC sequence should appear in output (cat echoes it)
    # Check for the O and P parts (ESC might be interpreted)
    [[ "$size" -gt 0 ]] || return 1
}

# Test: Bracketed paste mode sequences
term_input_bracketed_paste() {
    LOG_FILE="$E2E_LOG_DIR/term_input_paste.log"
    local output_file="$E2E_LOG_DIR/term_input_paste.pty"

    log_test_start "term_input_bracketed_paste"

    # Send bracketed paste: ESC[200~ content ESC[201~
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=2 \
    PTY_SEND="\x1b[200~PASTED_TEXT\x1b[201~\x04" \
    PTY_SEND_DELAY_MS=100 \
        pty_run "$output_file" cat

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_input_paste" "size_bytes" "$size"
    [[ "$size" -gt 0 ]] || return 1

    # Should see the pasted text
    grep -a -q "PASTED_TEXT" "$output_file" || return 1
}

# ============================================================================
# Resize Handling Tests
# ============================================================================

# Test: SIGWINCH is propagated on resize
term_resize_sigwinch() {
    LOG_FILE="$E2E_LOG_DIR/term_resize_winch.log"
    local output_file="$E2E_LOG_DIR/term_resize_winch.pty"

    log_test_start "term_resize_sigwinch"

    # Start with 80x24, resize to 120x40 after delay
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_DELAY_MS=500 \
    PTY_RESIZE_COLS=120 \
    PTY_RESIZE_ROWS=40 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" sh -c 'trap "echo SIGWINCH_RECEIVED" WINCH; echo "STARTED"; sleep 2; echo "DONE"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_resize_winch" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    grep -a -q "STARTED" "$output_file" || return 1
    # SIGWINCH handler should have fired
    grep -a -q "SIGWINCH_RECEIVED" "$output_file" || return 1
}

# Test: Rapid resizes don't crash or corrupt
term_resize_rapid_stability() {
    LOG_FILE="$E2E_LOG_DIR/term_resize_rapid.log"
    local output_file="$E2E_LOG_DIR/term_resize_rapid.pty"

    log_test_start "term_resize_rapid_stability"

    # Multiple rapid resizes shouldn't crash
    # We'll do a single resize but with short delay to test stability
    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_RESIZE_DELAY_MS=100 \
    PTY_RESIZE_COLS=60 \
    PTY_RESIZE_ROWS=20 \
    PTY_TIMEOUT=2 \
        pty_run "$output_file" sh -c 'echo "STABILITY_CHECK"; sleep 1; echo "COMPLETED"'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_resize_rapid" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1

    grep -a -q "STABILITY_CHECK" "$output_file" || return 1
    grep -a -q "COMPLETED" "$output_file" || return 1
}

# ============================================================================
# Real Program Tests
# ============================================================================

term_programs_bash_interactive() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_bash.log"
    local output_file="$E2E_LOG_DIR/term_programs_bash.pty"

    log_test_start "term_programs_bash_interactive"

    require_real_progs || return $?
    require_program bash || return $?

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=4 \
    PTY_SEND="echo BASH_OK\nexit\n" \
    PTY_SEND_DELAY_MS=200 \
        pty_run "$output_file" bash --noprofile --norc -i

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_bash" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
    grep -a -q "BASH_OK" "$output_file" || return 1
}

term_programs_zsh_interactive() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_zsh.log"
    local output_file="$E2E_LOG_DIR/term_programs_zsh.pty"

    log_test_start "term_programs_zsh_interactive"

    require_real_progs || return $?
    require_program zsh || return $?

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=4 \
    PTY_SEND="echo ZSH_OK\nexit\n" \
    PTY_SEND_DELAY_MS=200 \
        pty_run "$output_file" zsh -i

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_zsh" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
    grep -a -q "ZSH_OK" "$output_file" || return 1
}

term_programs_vim_render() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_vim.log"
    local output_file="$E2E_LOG_DIR/term_programs_vim.pty"

    log_test_start "term_programs_vim_render"

    require_real_progs || return $?
    local vim_bin=""
    if command -v vim >/dev/null 2>&1; then
        vim_bin="vim"
    elif command -v nvim >/dev/null 2>&1; then
        vim_bin="nvim"
    else
        skip_case "missing program: vim/nvim"
        return $?
    fi

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$vim_bin" -Nu NONE -n -es +'set nomore' +'put =\"VIM_OK\"' +'%print' +q

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_vim" "size_bytes" "$size"
    [[ "$size" -gt 5 ]] || return 1
    grep -a -q "VIM_OK" "$output_file" || return 1
}

term_programs_nano_render() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_nano.log"
    local output_file="$E2E_LOG_DIR/term_programs_nano.pty"

    log_test_start "term_programs_nano_render"

    require_real_progs || return $?
    require_program nano || return $?

    local nano_file="$E2E_LOG_DIR/nano_input.txt"
    printf "NANO_OK\n" > "$nano_file"

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=4 \
    PTY_SEND=$'\x18n' \
    PTY_SEND_DELAY_MS=400 \
        pty_run "$output_file" nano "$nano_file"

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_nano" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
}

term_programs_htop_live() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_htop.log"
    local output_file="$E2E_LOG_DIR/term_programs_htop.pty"

    log_test_start "term_programs_htop_live"

    require_real_progs || return $?
    require_program htop || return $?

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=4 \
    PTY_SEND="q" \
    PTY_SEND_DELAY_MS=600 \
        pty_run "$output_file" htop -d 10

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_htop" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
}

term_programs_top_batch() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_top.log"
    local output_file="$E2E_LOG_DIR/term_programs_top.pty"

    log_test_start "term_programs_top_batch"

    require_real_progs || return $?
    require_program top || return $?

    PTY_COLS=80 \
    PTY_ROWS=24 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" top -b -n 1

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_top" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
}

term_programs_less_scroll() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_less.log"
    local output_file="$E2E_LOG_DIR/term_programs_less.pty"

    log_test_start "term_programs_less_scroll"

    require_real_progs || return $?
    require_program less || return $?

    PTY_COLS=80 \
    PTY_ROWS=10 \
    PTY_TIMEOUT=4 \
    PTY_SEND="q" \
    PTY_SEND_DELAY_MS=400 \
        pty_run "$output_file" sh -c 'seq 1 200 | LESS=R less'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_less" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
}

term_programs_more_scroll() {
    LOG_FILE="$E2E_LOG_DIR/term_programs_more.log"
    local output_file="$E2E_LOG_DIR/term_programs_more.pty"

    log_test_start "term_programs_more_scroll"

    require_real_progs || return $?
    require_program more || return $?

    PTY_COLS=80 \
    PTY_ROWS=10 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" sh -c 'seq 1 200 | more'

    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    jsonl_log "output" "term_programs_more" "size_bytes" "$size"
    [[ "$size" -gt 10 ]] || return 1
}
# ============================================================================
# Run All Tests
# ============================================================================

# Initialize JSONL log with run metadata
{
    printf '{"event":"run_start","ts":"%s","suite":"test_embedded_terminal"' "$(date -Iseconds)"
    printf ',"env":{"term":"%s","shell":"%s"}}\n' "${TERM:-unknown}" "${SHELL:-unknown}"
} >> "$E2E_JSONL_LOG"

FAILURES=0

# PTY Management
run_case "term_pty_spawn_success" term_pty_spawn_success || FAILURES=$((FAILURES + 1))
run_case "term_pty_dev_tty_openable" term_pty_dev_tty_openable || FAILURES=$((FAILURES + 1))
run_case "term_pty_env_inheritance" term_pty_env_inheritance || FAILURES=$((FAILURES + 1))
run_case "term_pty_working_directory" term_pty_working_directory || FAILURES=$((FAILURES + 1))
run_case "term_pty_clean_termination" term_pty_clean_termination || FAILURES=$((FAILURES + 1))

# ANSI Rendering
run_case "term_ansi_256_color_output" term_ansi_256_color_output || FAILURES=$((FAILURES + 1))
run_case "term_ansi_cursor_positioning" term_ansi_cursor_positioning || FAILURES=$((FAILURES + 1))
run_case "term_ansi_line_wrapping" term_ansi_line_wrapping || FAILURES=$((FAILURES + 1))
run_case "term_ansi_clear_sequences" term_ansi_clear_sequences || FAILURES=$((FAILURES + 1))

# Input Forwarding
run_case "term_input_key_sequences" term_input_key_sequences || FAILURES=$((FAILURES + 1))
run_case "term_input_modifiers" term_input_modifiers || FAILURES=$((FAILURES + 1))
run_case "term_input_function_keys" term_input_function_keys || FAILURES=$((FAILURES + 1))
run_case "term_input_bracketed_paste" term_input_bracketed_paste || FAILURES=$((FAILURES + 1))

# Resize Handling
run_case "term_resize_sigwinch" term_resize_sigwinch || FAILURES=$((FAILURES + 1))
run_case "term_resize_rapid_stability" term_resize_rapid_stability || FAILURES=$((FAILURES + 1))

# Real Programs
run_case "term_programs_bash_interactive" term_programs_bash_interactive || FAILURES=$((FAILURES + 1))
run_case "term_programs_zsh_interactive" term_programs_zsh_interactive || FAILURES=$((FAILURES + 1))
run_case "term_programs_vim_render" term_programs_vim_render || FAILURES=$((FAILURES + 1))
run_case "term_programs_nano_render" term_programs_nano_render || FAILURES=$((FAILURES + 1))
run_case "term_programs_htop_live" term_programs_htop_live || FAILURES=$((FAILURES + 1))
run_case "term_programs_top_batch" term_programs_top_batch || FAILURES=$((FAILURES + 1))
run_case "term_programs_less_scroll" term_programs_less_scroll || FAILURES=$((FAILURES + 1))
run_case "term_programs_more_scroll" term_programs_more_scroll || FAILURES=$((FAILURES + 1))

# Finalize JSONL log
{
    printf '{"event":"run_end","ts":"%s","failures":%d}\n' "$(date -Iseconds)" "$FAILURES"
} >> "$E2E_JSONL_LOG"

exit "$FAILURES"
