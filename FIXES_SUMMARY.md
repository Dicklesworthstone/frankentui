# Fixes Summary - Session 2026-02-01 (Part 6)

## 20. Escape Sequence Sanitization Safety
**File:** `crates/ftui-render/src/sanitize.rs`
**Issue:** `skip_escape_sequence` was overly permissive, consuming bytes until a "final" byte or string terminator was found. This meant a malformed sequence (e.g., `\x1b[...]`) containing a newline would consume the newline and subsequent text, potentially hiding log lines or critical information from the user (a "log swallowing" vulnerability).
**Fix:** Updated `skip_escape_sequence` to strictly enforce parameter byte ranges for CSI (`0x20-0x3F`) and abort parsing upon encountering invalid control characters (like `\n`) in OSC/DCS sequences. This ensures that malformed escapes are terminated early, preserving the visibility of subsequent data.