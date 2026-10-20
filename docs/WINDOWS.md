# Windows Compatibility (v1)

This document records what FrankenTUI targets for Windows support in v1 and the
known limitations. Behavior varies by terminal emulator and by backend.

Status note: This project is still early. The items below are the **targeted**
v1 behavior, not a guarantee for every Windows terminal.

## Verification Status (DSR)

**Last validated:** 2026-10-19 (DSR `frankentui-windows-cross` receipt `20261019T133026-3574887` on `trj`; historical initial check 2026-02-03 under `bd-31go`)

### Cross-Platform DSR Matrix

| Check / Tool | Host / Target | Toolchain | Status |
|--------------|---------------|-----------|--------|
| `frankentui-full-gates` (`make gates`, `fmt`, `check`, `clippy -D warnings`, `rustdoc -D warnings`, `nextest`) | `trj` (`linux/amd64`) | pinned nightly (`rust-toolchain.toml`) | Validated (`20261013T174912-1105783`) |
| `frankentui-windows-cross` (`scripts/cross_platform_e2e.sh --windows-smoke`: `--target x86_64-pc-windows-gnu` workspace `clippy -D warnings`, `ftui-runtime --features native-backend` check, `ftui-tty`/`default_backend` test link, example builds, plus cross-platform unit tests and start/stop smoke) | `trj` (`linux/amd64` -> `x86_64-pc-windows-gnu`) | pinned nightly (`rust-toolchain.toml`) | Validated (`20261019T133026-3574887`) |
| Native Windows DSR host (`wlap`) live console execution | `wlap` (`windows/amd64`) | pinned nightly (`rust-toolchain.toml`) | Pending `wlap` Tailscale host reachability (`bd-g00-root-epic-ewths.6.6` / `.36.2`) |

See `.config/dsr-quality.yaml` (`frankentui-windows-cross`) and `scripts/cross_platform_e2e.sh` for the verification configuration.
PTY-based E2E tests do not run on Windows: this repository has no ConPTY driver (`tests/e2e/lib/pty.sh` and `ftui-pty` PTY spawning are Unix-only).

### Automated Testing

- **Cross-target Clippy & Build (`x86_64-pc-windows-gnu`)**: Full workspace `--all-targets -- -D warnings`, `ftui-runtime --features native-backend`, `ftui` examples (`minimal_inline.exe`, `agent_shell.exe`), and `default_backend` test binary compilation.
- **Cross-platform unit tests**:
  - Windows Terminal capability profile via `WT_SESSION` (`wt_session_profile_matches_windows_doc`)
  - Crossterm key normalization (`crossterm_key_event_maps_to_ftui_key`)
  - Headless simulator resize reflow (`simulator_resize_reflows_view`)
  - `FTUI_EXIT_AFTER_MS` parsing (`exit_after_env_parses_ms_and_rejects_garbage`)
  - Non-console error path (`with_config_reports_not_a_console_cleanly`)
  - `ftui-tty` availability and non-Unix `Unsupported` fallback (`backend_available_matches_cfg`, `non_unix_backend_reports_unsupported`)
  - Facade default backend selection (`default_backend_is_crossterm_on_windows`, `default_backend_is_native_on_unix`)
- **Start/stop smoke (`scripts/cross_platform_e2e.sh --windows-smoke`)**: Runs `minimal_inline`, `ftui-demo-showcase --screen=1`, and `agent_shell` under `FTUI_EXIT_AFTER_MS=300` (recording outcome `exit_after_ms` with cursor restore `\x1b[?25h`, or `not_a_console` when stdio is redirected without a controlling terminal) into `windows-smoke.jsonl`.

### Manual Validation Required

- Terminal-specific quirks (Terminal.app, iTerm2, Windows Terminal)
- Edge cases in legacy consoles (cmd.exe, ConHost)

## Backend on Windows

- **Crossterm by default in v1:** The `ftui` facade selects Crossterm (`crossterm-compat`) by default on non-Unix targets (`ftui_runtime::DEFAULT_BACKEND == "crossterm"`).
- **`ftui-tty` non-Unix shell:** On Windows, `ftui-tty` compiles as a documented shell (`ftui_tty::BACKEND_AVAILABLE == false`) and `TtyBackend::open(...)` returns `io::ErrorKind::Unsupported` (`"ftui-tty native backend is unix-only; use the crossterm-compat feature on Windows"`).
- **Cooperative smoke-test exit (`FTUI_EXIT_AFTER_MS`):** Setting `FTUI_EXIT_AFTER_MS=<ms>` arms a deadline inside `Program::run_event_loop` (and `agent_shell`) so interactive binaries exit cleanly with code `0` and full terminal teardown after `<ms>` milliseconds.
- **Native Windows backend deferred:** The dedicated `ftui-win32` backend remains deferred (`bd-lff4p.4.9`; see [ADR-004](adr/ADR-004-windows-v1-scope.md)).

## Supported Features (v1 target)

- Raw mode enter/exit with cleanup on panic (best effort via the backend)
- Basic key input handling (letters, arrows, modifiers)
- Resize events (where the backend provides them)
- Basic mouse support when the terminal supports SGR mouse encoding
- Color output:
  - 16 colors (baseline)
  - 256 colors (Windows Terminal, modern ConHost)
  - TrueColor (Windows Terminal)

## Known Limitations (v1)

- **Not a console:** When stdin/stdout is redirected (no console attached), `Program::with_config` / `App::run()` over Crossterm returns `io::ErrorKind::Unsupported` (`"stdin is not a console; run inside Windows Terminal, conhost, or use the headless simulator"`).
- DEC synchronized output (mode 2026) is not widely supported on Windows
- OSC 8 hyperlinks: Windows Terminal only; cmd.exe and legacy ConHost do not
- Bracketed paste: varies by terminal emulator
- Focus events: may be missing or unreliable in some terminals
- Kitty keyboard protocol: limited/absent support on Windows
- Scroll-region optimization (DECSTBM): behavior varies by terminal
- Mouse SGR mode may fall back to legacy encoding on some terminals

## Terminal Compatibility Matrix (Windows)

| Feature | Windows Terminal | cmd.exe | ConHost | PowerShell |
|---------|------------------|---------|---------|------------|
| TrueColor | Yes | No | Partial | Depends |
| OSC 8 Links | Yes | No | No | Partial |
| Mouse (SGR) | Yes | No | Partial | Partial |
| Sync Output (DEC 2026) | No | No | No | No |

## Configuration Recommendations

- Prefer **Windows Terminal** for the best experience.
- Use a Unicode-capable font (Cascadia Mono, JetBrains Mono, Fira Code).
- If using legacy consoles, ensure UTF-8 mode is enabled.
- `WT_SESSION` (Windows Terminal) is treated as authoritative even if `TERM` is missing.
- If `TERM`/`COLORTERM` are missing and `WT_SESSION` is not set, expect reduced feature detection.

## Troubleshooting

- Colors do not show: verify terminal supports the color depth; check `COLORTERM`.
- Mouse not working: enable mouse support in the terminal settings.
- Cleanup not working: legacy ConHost may not restore modes reliably.
- Unicode display broken: verify font and codepage; avoid cmd.exe for complex text.

## Future Improvements

- Deeper ConHost support (where technically possible)
- WSL-specific notes and validation
- Expanded PTY tests on Windows CI runners
- More explicit capability probes for missing env vars

## Deferred Native Backend Strategy (bd-lff4p.4.9)

This is the implementation plan for Windows **after** the Unix backend (`ftui-tty`)
is stable. It is intentionally non-blocking for current Unix + web delivery.

### Strategy Decision

- Keep backend abstraction in `ftui-backend` as the stable seam.
- Implement Windows support as a dedicated crate (`ftui-win32`, name tentative),
  not by re-expanding `crossterm` usage in runtime/core.
- Reuse existing shared layers unchanged:
  - `ftui-core` event/capability model
  - `ftui-render` frame/diff/presenter
  - `ftui-runtime` program loop + one-writer ownership model

### Dependency Plan

| Phase | Goal | Primary Dependencies | Notes |
|------|------|----------------------|-------|
| 0 (current) | Keep Windows CI green while Unix backend evolves | Existing workspace deps only | No new Windows-only runtime dependency yet |
| 1 | Introduce backend crate skeleton + RAII lifecycle | `windows-sys` (or `windows`) APIs for console mode + handles | Mirror `TerminalSession` cleanup invariants |
| 2 | Input path parity (key/mouse/resize/focus) | Win32 console input records + ConPTY/terminal mode probes | Normalize to `ftui_core::event::Event` |
| 3 | Output + capability parity | VT enablement APIs, cursor/screen mode toggles, synchronized flushing | Preserve one-writer rule from `TerminalWriter` |
| 4 | Validation + hardening | Windows CI matrix + deterministic snapshot/e2e fixtures | Add terminal-specific fixtures (Windows Terminal + legacy paths) |

### Scope Boundaries (Non-Blocking)

- Work in this plan must **not** block:
  - `bd-lff4p.2*` (web renderer/input path)
  - `bd-lff4p.3*` (ftui-web WASM path)
  - `bd-lff4p.10*` (remote protocol/PTY bridge)
- Unix-first milestones remain priority; Windows native backend starts only after:
  1. Unix backend API stabilizes,
  2. parity tests for core lifecycle pass consistently,
  3. CI signal for existing targets is green.

### Risk Register (Windows-Specific)

- Console capability fragmentation (Windows Terminal vs ConHost/cmd.exe):
  mitigate by capability-driven branches and strict downgrade paths.
- Input encoding differences:
  mitigate with explicit normalization tests (modifiers, IME, surrogate pairs).
- Cleanup regressions on abrupt exits:
  mitigate with RAII + panic-path tests mirroring Unix teardown guarantees.

## Cross-References

- [ADR-004 (Windows v1 scope)](adr/ADR-004-windows-v1-scope.md) — ACCEPTED (2026-10-19)
- [Terminal compatibility matrix](terminal-compatibility.md) (`bd-1un`)
- Capability detection: `crates/ftui-core/src/terminal_capabilities.rs`

## Validation Checklist

- [x] Linux tests passing (DSR `frankentui-full-gates` on `trj`)
- [x] Windows `--target x86_64-pc-windows-gnu` workspace Clippy (`-D warnings`), test compilation, and examples passing (DSR `frankentui-windows-cross` receipt `20261019T133026-3574887`)
- [ ] Live native Windows DSR host (`wlap`) test and start/stop execution (`bd-g00-root-epic-ewths.6.6` / `bd-g00-root-epic-ewths.36.2`)
- [x] Platform-specific capability detection tested (`wt_session_profile_matches_windows_doc`)
- [x] Snapshot tests and headless simulator resize reflow tested (`simulator_resize_reflows_view`)
- [x] `docs/WINDOWS.md` updated with DSR validation status
