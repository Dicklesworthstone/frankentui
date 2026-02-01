# ADR-005: One-Writer Rule Enforcement

Status: PROPOSED

## Context
In inline mode, FrankenTUI must preserve scrollback and keep cursor state stable while rendering UI and streaming logs.

Terminals are a shared mutable resource. Concurrent writers are not safe:
- cursor position becomes undefined
- partial escape sequences corrupt output
- UI and logs interleave unpredictably

## Decision
Enforce the one-writer rule through ownership + routing:
- A `TerminalSession` (or equivalent) owns terminal state and output handles.
- A `TerminalWriter` is the single gate for:
  - presenting UI frames
  - writing logs
  - toggling terminal modes
- Provide supported routing patterns for in-process logs and subprocess output.

## Supported Routing Patterns

### Pattern A: LogSink (In-Process)
Provide a `LogSink: Write` (or similar) that applications use instead of `println!()`.
All output goes through the same writer and policy.

### Pattern B: PTY Capture (Subprocess Output)
Run subprocess tools under a PTY and forward their output through the `TerminalWriter`.
This avoids terminal corruption and preserves the user-facing log story.

### Pattern C: Stdio Capture (Optional)
Best-effort capture/forwarding for accidental stdout writes by libraries.
This is inherently leaky (not all output is capturable) and must be feature-gated.

## Consequences
- Applications must use ftui output APIs when ftui is active.
- Libraries that write to stdout/stderr can still break guarantees.
- We must document the unsupported behavior clearly.

## Test Plan
- PTY tests that validate stability under sustained log output + UI redraw.
- Tests for supported routing patterns (LogSink, PTY capture).
- Documentation describes undefined behavior when violated.
