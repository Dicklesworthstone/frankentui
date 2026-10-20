# ADR-004: Windows v1 Scope

Status: ACCEPTED (2026-10-19 — crossterm default on Windows for v1; native `ftui-win32` deferred; live Windows DSR host `wlap` run tracked by `bd-g00-root-epic-ewths.6.6` / `.36.2`)

## Context
Windows terminal behavior differs from Unix in ways that affect terminal UI libraries:
- feature support is inconsistent across terminal emulators
- certain DEC private modes are not supported
- event delivery and input nuances differ

We must decide what “Windows support” means for FrankenTUI v1.

## Decision
Windows is supported in v1 with documented limitations:
- **Backend:** Crossterm (`crossterm-compat`) is selected by default on Windows (`ftui_runtime::DEFAULT_BACKEND == "crossterm"`); `ftui-tty` compiles as an `Unsupported` stub (`ftui_tty::BACKEND_AVAILABLE == false`); the native `ftui-win32` backend is deferred (`bd-lff4p.4.9`).
- core functionality works (raw mode lifecycle, basic input, resize, color where exposed)
- advanced terminal features are best-effort and may be unavailable (sync output, OSC 8, Kitty protocols)
- when stdin/stdout is not a console, `Program::with_config` returns `io::ErrorKind::Unsupported` (`NOT_A_CONSOLE_MESSAGE`)

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
- DSR verification includes `--target x86_64-pc-windows-gnu` cross-target Clippy/build/test-link and cross-platform unit/smoke tests (`frankentui-windows-cross`).

## Test Plan
- DSR `frankentui-windows-cross` (`scripts/cross_platform_e2e.sh --windows-smoke`, receipt `20261019T133026-3574887`): workspace Clippy (`-D warnings`), `ftui-runtime --features native-backend` check, `ftui-tty` and `default_backend` test compilation, and example builds for `x86_64-pc-windows-gnu`.
- Cross-platform correctness tests (`wt_session_profile_matches_windows_doc`, `crossterm_key_event_maps_to_ftui_key`, `simulator_resize_reflows_view`, `exit_after_env_parses_ms_and_rejects_garbage`, `with_config_reports_not_a_console_cleanly`, `default_backend_is_crossterm_on_windows`, `non_unix_backend_reports_unsupported`).
- [`docs/WINDOWS.md`](../WINDOWS.md) documents what works, what is limited, and the deferred native backend strategy.

## History
- **2026-02:** Proposed during initial architecture phase (`bd-10i.10.4`).
- **2026-10-19:** Accepted with Crossterm-only v1 Windows backend, `FTUI_EXIT_AFTER_MS` smoke hook, `NOT_A_CONSOLE_MESSAGE` guard, and DSR `frankentui-windows-cross` verification (`20261019T133026-3574887`).
