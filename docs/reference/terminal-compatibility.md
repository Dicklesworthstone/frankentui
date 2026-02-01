# Terminal Compatibility Matrix

This document summarizes FrankenTUI feature support across common terminals and
multiplexers. It is a user-facing guide to what works where and what ftui will
enable by default.

## Compatibility Matrix

Legend: Yes = supported, No = not supported, Partial = limited/quirks.

| Feature | Kitty | WezTerm | Alacritty | Ghostty | iTerm2 | GNOME Term | Windows Terminal |
| --- | --- | --- | --- | --- | --- | --- | --- |
| True color (24-bit) | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Sync output (DEC 2026) | Yes | Yes | Yes | Yes | No | No | No |
| OSC 8 hyperlinks | Yes | Yes | Yes | Yes | Yes | Yes | No |
| Kitty keyboard protocol | Yes | Yes | No | Yes | No | No | No |
| Kitty graphics | Yes | Yes | No | Yes | No | No | No |
| Sixel | No | Yes | No | No | Yes | No | No |
| Focus events | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Bracketed paste | Yes | Yes | Yes | Yes | Yes | Yes | Yes |

Notes:
- Multiplexers (tmux/screen/zellij) can reduce effective support even when the
  underlying terminal supports a feature.
- ftui enables sync output, OSC 8, and kitty protocol features only when
  `TerminalCapabilities` indicates they are safe.

## Feature Details

### True color (24-bit RGB)
- Detection: `COLORTERM=truecolor` or `COLORTERM=24bit`.
- Fallback: 256-color or 16-color palette.

### Sync output (DEC 2026)
- Purpose: atomically update frames to reduce flicker.
- Detection: known terminal capability or allowlist.
- Default: enabled only when safe; disabled in multiplexers.

### OSC 8 hyperlinks
- Purpose: clickable links for help, logs, and UI metadata.
- Format: `ESC ] 8 ; ; URL ESC \` ... `ESC ] 8 ; ; ESC \`.
- Windows Terminal: not supported.

### Kitty keyboard protocol
- Purpose: richer key data (press/release/repeat, modifiers).
- Limited adoption; only enabled when explicitly detected.

### Sixel / Kitty graphics
- Purpose: inline graphics.
- Feature-gated; not required for kernel correctness.

## Multiplexer Notes

### tmux
- Passthrough required for OSC and sync output.
- Sync output is disabled by default in tmux.
- OSC 8 may work on newer tmux, but ftui is conservative by default.
- Detection: `TMUX` environment variable.

### screen
- Limited support for modern features.
- Sync output and OSC 8 passthrough are unreliable.
- Detection: `STY` environment variable.

### zellij
- Better passthrough than tmux/screen, but still conservative.
- Detection: `ZELLIJ` environment variable.

## Windows Considerations

- Windows Terminal supports ANSI and true color.
- OSC 8 hyperlinks and sync output are not supported.
- Raw mode and input handling rely on crossterm on Windows.
- conhost / legacy consoles are out of scope for v1.

## Inline Mode Notes

- Inline mode is designed to work everywhere.
- Scroll region optimization is optional and disabled in multiplexers.
- Cursor save/restore is universally supported.

## Recommended Test Matrix

For CI or manual verification, test against:
1. Kitty (max feature surface)
2. WezTerm (cross-platform)
3. Alacritty (minimal, popular)
4. Windows Terminal (Windows v1 scope)

## Known Limitations

- Multiplexers can disable or break passthrough for advanced sequences.
- Feature detection is intentionally conservative to preserve correctness.
- Some features are feature-gated and not part of the kernel contract.
