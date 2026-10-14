# ADR-006: Untrusted Output Policy (Sanitize-by-Default)

Status: ACCEPTED (2026-10-14)

## Context
Agent harness UIs display tool output, LLM streams, and logs.

Untrusted output can smuggle control sequences (ANSI/CSI/OSC/DCS/APC) that:
- manipulate terminal state
- deceive users (fake prompts)
- persist changes after the app exits

This is a real security concern.

## Decision
Sanitize by default:
- Any text flowing through log paths or user-provided content is sanitized.
- Raw passthrough is explicitly opt-in.

## What Gets Sanitized
- ESC (0x1B) and all CSI/OSC/DCS/APC/PM/SOS sequences
- C0 controls except TAB (0x09), LF (0x0A), CR (0x0D)
- C1 controls (0x80..=0x9F)

Semi-trusted mode: SGR-only (`write_log_sgr_only` / `LogSink::sgr_only` / `sanitize_sgr_only`):
- Allowlist: `CSI [0-9;:]* m` up to 64 bytes (`\x1b[...m`).
- Everything else is stripped, including private CSI prefixes (`?`, `>`, `<`, `=`), intermediate bytes, cursor movement, DECSET/DECRST, and OSC 8 hyperlinks.

## Sanitization Strategy
Default strategy: STRIP.
- removes control sequences entirely
- keeps output readable and safe

(Other strategies may be offered later: escape or replace.)

## Delivered API
- `TerminalWriter::write_log(&str)` / `LogSink::new(&mut writer)` / `Cmd::log(text)` / `SanitizeMode::Strip`: strips all escape sequences and unsafe controls by default.
- `TerminalWriter::write_log_sgr_only(&str)` / `LogSink::sgr_only(&mut writer)` / `Cmd::log_sgr_only(text)` / `SanitizeMode::SgrOnly`: retains only bounded SGR color/attribute sequences (`CSI [0-9;:]* m`, <= 64 bytes) and appends a per-line `\x1b[0m` reset before each newline so colored log output cannot bleed into subsequent lines or UI chrome.
- `TerminalWriter::write_log_raw(&str)` / `LogSink::raw(&mut writer)` / `Cmd::log_raw(text)` / `SanitizeMode::Raw`: explicit raw passthrough for trusted sources only.
- `TerminalWriter::write_log_text(&ftui_render::sanitize::Text)` and `ftui_render::sanitize::{sanitize, sanitize_sgr_only, sanitize_with, SanitizeMode, Text}`: typed wrappers (`Text::sanitized`, `Text::sgr_only`, `Text::raw`) that bind sanitization policy to the value.

## Decisions Made During Implementation
- **No global switch for raw mode:** Raw passthrough cannot be enabled via environment variable or global configuration; every raw write requires an explicit call to `write_log_raw`, `LogSink::raw`, `Cmd::log_raw`, or `Text::raw`.
- **Per-line SGR reset:** `write_log_sgr_only` and `LogSink::sgr_only` append `\x1b[0m` at the end of every non-empty line so unclosed SGR attributes from tool output cannot leak into following log lines or the inline UI region.
- **Chunk-boundary reassembly in `LogSink`:** `LogSink::new` and `LogSink::sgr_only` buffer incomplete UTF-8 sequences and split escape sequences across `std::io::Write::write` calls before sanitizing, preventing split-write smuggling.
- **Audit trace on raw writes:** `write_log_raw` emits a `tracing::debug!(target: "ftui.sanitize", mode = "raw", byte_len = ...)` event for every non-empty raw write.

## Consequences
- User content is safe by default.
- Legitimate SGR color output from semi-trusted tools can use `write_log_sgr_only` / `LogSink::sgr_only` without exposing cursor, screen, or title control sequences.
- Raw passthrough is conspicuous at call sites and traceable at runtime.

## Test Plan
- PTY tests that feed adversarial sequences (`ADVERSARIAL_PAYLOADS`) into logs and assert terminal invariants: `crates/ftui-harness/tests/pty_terminal_lifecycle.rs` (`pty_log_injection_preserves_text_style_and_terminal_state`, `pty_model_log_commands_preserve_text_style_and_terminal_state`, `pty_log_raw_requires_explicit_opt_in_and_restores_termios`, `pty_model_log_raw_is_explicit_and_restores_termios`, `raw_log_emits_mode_and_byte_count_trace`, `alt_screen_mode_is_the_only_path_to_1049h`).
- Property tests ensuring inline mode never clears the screen (`\x1b[2J`, `\x1b[3J`, `\x1b[?1049h`, `\x1bc`) and SGR-only mode never leaks non-SGR escapes: `crates/ftui-harness/tests/proptest_flicker_invariants.rs` (`inline_never_clears_screen`) and `crates/ftui-render/src/sanitize.rs` (`proptest_adversarial::*`).
- Fuzz tests with arbitrary bytes for strip and SGR-only paths: `fuzz/fuzz_targets/fuzz_sanitize.rs` and `fuzz/fuzz_targets/fuzz_sanitize_sgr_only.rs`.

## History
Original API sketch (retained for historical context):
- `Status: PROPOSED`
- Optional semi-trusted mode (future): allow SGR-only.
- `write_log(...)` / `Text::sanitized(...)` are safe by default
- `write_raw(...)` / `Text::raw(...)` require explicit opt-in
