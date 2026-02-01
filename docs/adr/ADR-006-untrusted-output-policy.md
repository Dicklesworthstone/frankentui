# ADR-006: Untrusted Output Policy (Sanitize-by-Default)

Status: PROPOSED

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
- ESC (0x1B) and all CSI/OSC/DCS/APC sequences
- C0 controls except TAB (0x09), LF (0x0A), CR (0x0D)

Optional semi-trusted mode (future): allow SGR-only.

## Sanitization Strategy
Default strategy: STRIP.
- removes control sequences entirely
- keeps output readable and safe

(Other strategies may be offered later: escape or replace.)

## API Sketch
- `write_log(...)` / `Text::sanitized(...)` are safe by default
- `write_raw(...)` / `Text::raw(...)` require explicit opt-in

## Consequences
- User content is safe by default.
- Some legitimate ANSI in logs will be stripped unless explicitly opted-in.

## Test Plan
- PTY tests that feed adversarial sequences into logs and assert terminal invariants.
- Fuzz tests with random bytes for log paths.
- Tests ensuring no state leakage after malicious content.
