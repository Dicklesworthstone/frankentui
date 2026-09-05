#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$SCRIPT_DIR/../lib"

# shellcheck source=/dev/null
source "$LIB_DIR/common.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/logging.sh"
# shellcheck source=/dev/null
source "$LIB_DIR/pty.sh"

FIXTURE_DIR="$E2E_ROOT/fixtures"

HARNESS_CASES=(
    unicode_basic_ascii
    unicode_accented
    unicode_wide_cjk
    unicode_emoji
    unicode_mixed_content
)

DEMO_CASES=(
    unicode_demo_ascii_icons
    unicode_demo_emoji_icons
)

ensure_demo_bin() {
    local target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
    local bin="${E2E_DEMO_BIN:-$target_dir/debug/ftui-demo-showcase}"
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    log_info "Building ftui-demo-showcase (debug)..." >&2
    (cd "$PROJECT_ROOT" && cargo build -p ftui-demo-showcase >/dev/null)
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return 0
    fi
    return 1
}

require_harness_bin() {
    if [[ -x "${E2E_HARNESS_BIN:-}" ]]; then
        return 0
    fi
    SKIP_REASON="ftui-harness binary missing"
    return 2
}

require_demo_bin() {
    if [[ -n "${E2E_DEMO_BIN_RESOLVED:-}" && -x "$E2E_DEMO_BIN_RESOLVED" ]]; then
        return 0
    fi
    if E2E_DEMO_BIN_RESOLVED="$(ensure_demo_bin)"; then
        return 0
    fi
    SKIP_REASON="ftui-demo-showcase binary missing"
    return 2
}

run_case() {
    local name="$1"
    shift
    local start_ms
    start_ms="$(e2e_monotonic_ms)" || return 2

    if "$@"; then
        local end_ms
        end_ms="$(e2e_monotonic_ms)" || return 2
        local duration_ms=$((end_ms - start_ms))
        log_test_pass "$name"
        record_result "$name" "passed" "$duration_ms" "$LOG_FILE"
        return 0
    fi

    local exit_code=$?
    local end_ms
    end_ms="$(e2e_monotonic_ms)" || return 2
    local duration_ms=$((end_ms - start_ms))
    if [[ "$exit_code" -eq 2 ]]; then
        local reason="${SKIP_REASON:-skipped}"
        log_test_skip "$name" "$reason"
        record_result "$name" "skipped" "$duration_ms" "$LOG_FILE" "$reason"
        SKIP_REASON=""
        return 0
    fi
    log_test_fail "$name" "unicode assertions failed"
    record_result "$name" "failed" "$duration_ms" "$LOG_FILE" "unicode assertions failed"
    return 1
}

# Test: Basic ASCII content renders without issues
unicode_basic_ascii() {
    LOG_FILE="$E2E_LOG_DIR/unicode_basic_ascii.log"
    local output_file="$E2E_LOG_DIR/unicode_basic_ascii.pty"

    log_test_start "unicode_basic_ascii"
    require_harness_bin || return 2

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_LINES=10 \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # ASCII log lines should appear in output
    grep -a -q "Log line" "$output_file" || return 1
    # Status bar text should be present
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "Basic ASCII rendering verified"
}

# Test: Accented characters render correctly
unicode_accented() {
    LOG_FILE="$E2E_LOG_DIR/unicode_accented.log"
    local output_file="$E2E_LOG_DIR/unicode_accented.pty"

    log_test_start "unicode_accented"
    require_harness_bin || return 2

    # Create a temp log file with accented content
    local log_content
    log_content="$(mktemp)"
    printf 'café résumé naïve\n' > "$log_content"
    printf 'Héllo àccénted wörld\n' >> "$log_content"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_FILE="$log_content" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    rm -f "$log_content"

    # The output should contain the accented text (rendered through the PTY)
    # Accented chars are single-width, so should pass through.
    grep -a -q "caf" "$output_file" || return 1

    # Output should be substantial (app rendered without crashing on accented input)
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    log_debug "Accented character rendering verified"
}

# Test: CJK wide characters do not crash the renderer
unicode_wide_cjk() {
    LOG_FILE="$E2E_LOG_DIR/unicode_wide_cjk.log"
    local output_file="$E2E_LOG_DIR/unicode_wide_cjk.pty"

    log_test_start "unicode_wide_cjk"
    require_harness_bin || return 2

    local log_content
    log_content="$(mktemp)"
    printf '日本語テスト\n' > "$log_content"
    printf '中文测试内容\n' >> "$log_content"
    printf '한국어 테스트\n' >> "$log_content"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_FILE="$log_content" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    rm -f "$log_content"

    # The app must not crash when rendering wide characters.
    # Verify the output file has content (render cycles completed).
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Status bar should still render (app didn't panic on wide chars)
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "CJK wide character rendering verified (no crash)"
}

# Test: Emoji characters do not crash the renderer
unicode_emoji() {
    LOG_FILE="$E2E_LOG_DIR/unicode_emoji.log"
    local output_file="$E2E_LOG_DIR/unicode_emoji.pty"

    log_test_start "unicode_emoji"
    require_harness_bin || return 2

    local log_content
    log_content="$(mktemp)"
    printf '🎉 Party time!\n' > "$log_content"
    printf '🚀 Launch 🌍 Earth\n' >> "$log_content"
    printf '✅ Done ❌ Failed ⚠️ Warning\n' >> "$log_content"

    FTUI_HARNESS_EXIT_AFTER_MS=800 \
    FTUI_HARNESS_LOG_FILE="$log_content" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=3 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    rm -f "$log_content"

    # The app must not crash when rendering emoji.
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Status bar should still render
    grep -a -q "claude-3.5" "$output_file" || return 1

    log_debug "Emoji rendering verified (no crash)"
}

# Test: Mixed content (ASCII + Unicode + Emoji) in a single session
unicode_mixed_content() {
    LOG_FILE="$E2E_LOG_DIR/unicode_mixed_content.log"
    local output_file="$E2E_LOG_DIR/unicode_mixed_content.pty"

    log_test_start "unicode_mixed_content"
    require_harness_bin || return 2

    FTUI_HARNESS_EXIT_AFTER_MS=1000 \
    FTUI_HARNESS_LOG_FILE="$FIXTURE_DIR/unicode_lines.txt" \
    FTUI_HARNESS_SUPPRESS_WELCOME=1 \
    PTY_TIMEOUT=4 \
        pty_run "$output_file" "$E2E_HARNESS_BIN"

    # The app must not crash on the full unicode fixture file.
    local size
    size=$(wc -c < "$output_file" | tr -d ' ')
    [[ "$size" -gt 200 ]] || return 1

    # Status bar should still render
    grep -a -q "claude-3.5" "$output_file" || return 1

    # At least some ASCII content from the fixture should appear
    grep -a -q "Hello" "$output_file" || grep -a -q "ASCII" "$output_file" || return 1

    log_debug "Mixed unicode content rendering verified"
}

# Test: Demo file browser renders ASCII icons when emoji disabled
unicode_demo_ascii_icons() {
    LOG_FILE="$E2E_LOG_DIR/unicode_demo_ascii_icons.log"
    local output_file="$E2E_LOG_DIR/unicode_demo_ascii_icons.pty"

    log_test_start "unicode_demo_ascii_icons"
    require_demo_bin || return 2

    FTUI_DEMO_SCREEN=9 \
    FTUI_DEMO_SCREEN_MODE=alt \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SEED=0 \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    FTUI_GLYPH_MODE=ascii \
    FTUI_GLYPH_EMOJI=0 \
    FTUI_NO_EMOJI=1 \
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_DEMO_BIN_RESOLVED"

    grep -a -q "DR my-app" "$output_file" || grep -a -q "RS main.rs" "$output_file" || return 1
    grep -a -q "🦀" "$output_file" && return 1
    grep -a -q "📁" "$output_file" && return 1

    log_debug "Demo file browser ASCII icons verified"
}

# Test: Demo file browser renders emoji icons when enabled
unicode_demo_emoji_icons() {
    LOG_FILE="$E2E_LOG_DIR/unicode_demo_emoji_icons.log"
    local output_file="$E2E_LOG_DIR/unicode_demo_emoji_icons.pty"

    log_test_start "unicode_demo_emoji_icons"
    require_demo_bin || return 2

    FTUI_DEMO_SCREEN=9 \
    FTUI_DEMO_SCREEN_MODE=alt \
    FTUI_DEMO_DETERMINISTIC=1 \
    FTUI_DEMO_SEED=0 \
    FTUI_DEMO_EXIT_AFTER_MS=1200 \
    FTUI_GLYPH_MODE=unicode \
    FTUI_GLYPH_EMOJI=1 \
    PTY_COLS=120 \
    PTY_ROWS=40 \
    PTY_TIMEOUT=6 \
        pty_run "$output_file" "$E2E_DEMO_BIN_RESOLVED"

    grep -a -q "🦀" "$output_file" || grep -a -q "📁" "$output_file" || return 1

    log_debug "Demo file browser emoji icons verified"
}

FAILURES=0
run_case "unicode_basic_ascii" unicode_basic_ascii         || FAILURES=$((FAILURES + 1))
run_case "unicode_accented" unicode_accented               || FAILURES=$((FAILURES + 1))
run_case "unicode_wide_cjk" unicode_wide_cjk              || FAILURES=$((FAILURES + 1))
run_case "unicode_emoji" unicode_emoji                     || FAILURES=$((FAILURES + 1))
run_case "unicode_mixed_content" unicode_mixed_content     || FAILURES=$((FAILURES + 1))
run_case "unicode_demo_ascii_icons" unicode_demo_ascii_icons || FAILURES=$((FAILURES + 1))
run_case "unicode_demo_emoji_icons" unicode_demo_emoji_icons || FAILURES=$((FAILURES + 1))
exit "$FAILURES"
