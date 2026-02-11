# Screen Modes: Inline vs Alt-Screen

FrankenTUI supports two screen modes. The choice has real user-facing
consequences for scrollback, cursor behavior, and cleanup requirements.

## TL;DR

- Inline mode (default): preserves scrollback, UI pinned to a fixed region.
- Alt-screen mode: full-screen UI, no scrollback after exit.

## Inline Mode

Definition: The UI renders in the normal terminal buffer, above the
current prompt, while logs and tool output scroll normally.

ASCII sketch:

```
[prior shell output]
[prior shell output]
[log line 1]
[log line 2]
+----------------------------+
| Status: Running tool...    |
| > Command: _               |
+----------------------------+
```

Key behaviors:
- Scrollback is preserved.
- Logs remain visible after exit.
- UI occupies a fixed-height region anchored to the bottom.
- Cursor must be restored after every present call.

Configuration:

```rust
use ftui_runtime::{ProgramConfig, ScreenMode};

fn main() {
    let mut config = ProgramConfig::default();
    config.screen_mode = ScreenMode::Inline { ui_height: 4 };
    // pass config into Program::new(...)
}
```

## Alt-Screen Mode

Definition: The UI renders in the alternate buffer. The terminal restores
the previous screen when the app exits.

ASCII sketch:

```
+----------------------------+
| Full-screen UI             |
| (no scrollback)            |
|                            |
|                            |
+----------------------------+
```

Key behaviors:
- Scrollback is not preserved.
- Full-screen clears are safe.
- Cursor management is simpler (but still must be cleaned up on exit).

Configuration:

```rust
use ftui_runtime::{ProgramConfig, ScreenMode};

fn main() {
    let mut config = ProgramConfig::default();
    config.screen_mode = ScreenMode::AltScreen;
    // pass config into Program::new(...)
}
```

## Trade-off Matrix

| Feature | Inline | Alt-screen |
| --- | --- | --- |
| Scrollback preserved | Yes | No |
| Full-screen clear safe | No | Yes |
| Logs visible after exit | Yes | No |
| Cursor management complexity | High | Medium |
| Classic TUI feel | No | Yes |
| Best for agent harness | Yes | No |

## Mouse Capture Policy

Mouse capture policy is explicit in `ProgramConfig`:

- `MouseCapturePolicy::Auto` (default): `AltScreen => ON`, `Inline/InlineAuto => OFF`
- `MouseCapturePolicy::On`: always ON
- `MouseCapturePolicy::Off`: always OFF

This keeps inline mode scrollback-safe by default while preserving classic
full-screen mouse behavior in alt-screen mode.

```rust
use ftui_runtime::{MouseCapturePolicy, ProgramConfig, ScreenMode};

let config = ProgramConfig::default()
    .with_mouse_capture_policy(MouseCapturePolicy::Auto)
    .with_mouse_enabled(false); // force OFF if needed

assert!(!config.resolved_mouse_capture()); // default inline => off
assert!(ProgramConfig::fullscreen().resolved_mouse_capture()); // auto alt => on
```

## When to Use Inline Mode

Use inline mode when:
- You need log history after exit.
- The UI is a pinned status or prompt region.
- You are building an agent harness or REPL.

Examples:
- Tool execution UI with streaming logs.
- Build/test runners that should preserve scrollback.
- REPLs that must not destroy terminal history.

## When to Use Alt-Screen Mode

Use alt-screen mode when:
- The UI is a full-screen dashboard or editor.
- Scrollback is irrelevant or distracting.
- You want classic TUI behavior (vim, htop).

## Mixed Strategy (Inline + Alt-Screen)

Some apps may start inline, then temporarily switch to alt-screen for
modal experiences (file picker, help overlay), and then return inline.

Pseudo-flow:

```
Inline: stream logs + pinned UI
User opens modal -> enter alt-screen
User closes modal -> return to inline
```

Note: A dedicated runtime command API for mode switching is planned.
For now, prefer a fixed mode per session.

## Implementation Contracts

Inline mode contract:
- Never emit full-screen clear (ESC [ 2 J).
- Only clear the UI region.
- Cursor must be restored after every present.
- All output must go through the TerminalWriter (one-writer rule).

Alt-screen mode contract:
- Enter on start (smcup) and exit on shutdown (rmcup).
- Cleanup must run on normal exit and panic paths.
- Full-screen clear is allowed.

## Common Mistakes

Mistake 1: Full-screen clear in inline mode

Wrong:
```
ESC [ 2 J   // destroys scrollback
```

Right:
```
Clear only the UI region with per-line erase
```

Mistake 2: Expecting scrollback in alt-screen

If you use alt-screen, the scrollback buffer is separate and disappears
when the app exits.

Mistake 3: Fixed inline anchor during resize

Inline UI must recompute its anchor on resize. Do not hardcode the
start row.

Mistake 4: Printing to stdout outside the runtime

Direct prints will corrupt cursor state and interleave with UI output.
Route output through the runtime or LogSink.

## Debugging Screen Mode Issues

Symptom: Scrollback is gone after exit
- Likely cause: Alt-screen mode enabled
- Fix: Use ScreenMode::Inline

Symptom: Cursor jumps or drifts
- Likely cause: Out-of-band stdout writes or missing cursor restore
- Fix: Enforce one-writer rule, verify presenter restores cursor

Symptom: UI overwrites logs in inline mode
- Likely cause: ui_height too small or anchor not updated
- Fix: Increase ui_height and update on resize

## Related ADRs

- ADR-001: Inline Mode Strategy (docs/adr/ADR-001-inline-mode.md)
