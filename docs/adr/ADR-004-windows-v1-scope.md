# ADR-004: Windows v1 Scope

Status: PROPOSED

## Context
Windows terminal behavior differs from Unix in ways that affect terminal UI libraries:
- feature support is inconsistent across terminal emulators
- certain DEC private modes are not supported
- event delivery and input nuances differ

We must decide what “Windows support” means for FrankenTUI v1.

## Decision
Windows is supported in v1 with documented limitations:
- core functionality works (raw mode lifecycle, basic input, resize, color where exposed)
- advanced terminal features are best-effort and may be unavailable (sync output, OSC 8, Kitty protocols)

## Supported in v1
- Raw mode enter/exit and cleanup on panic
- Key input and resize events
- Basic mouse (where backend supports it)
- Color where available (16/256/truecolor as exposed)

## Best-Effort / May Be Missing in v1
- DEC synchronized output (mode 2026)
- OSC 8 hyperlinks
- Bracketed paste differences across terminals
- Focus events differences
- Kitty keyboard protocol

## Rationale
- The primary near-term target is an agent harness UI (commonly Unix/macOS).
- Full Windows feature parity would delay v1 significantly.
- Honest docs + reliable cleanup beats unstable half-support.

## Consequences
- Windows users may see degraded functionality.
- Documentation must be explicit and practical.
- CI should include Windows builds early (even if some integration tests are skipped).

## Test Plan
- Windows CI build passes.
- Minimal correctness tests run on Windows (raw mode lifecycle, basic input decode, resize).
- `docs/windows.md` (or equivalent) documents what works and what doesn’t.
