# Glossary

This glossary defines terms used throughout FrankenTUI's codebase and documentation.

---

## Core Types

| Term | Definition |
|------|------------|
| **Cell** | Single grid position containing content (char or grapheme ID), foreground color, background color, and style attributes. 16 bytes. |
| **CellContent** | 4-byte discriminated union: either a Unicode scalar value (char) or a GraphemeId reference. |
| **CellAttrs** | Style flags (u16) + link ID (u16) packed into 4 bytes. |
| **Buffer** | 2D array of Cells representing display state. Row-major storage for cache efficiency. |
| **Frame** | Buffer + cursor position + hit grid for a render pass. Represents the desired terminal state. |
| **Diff** | Set of (x, y) positions that changed between front and back buffers. |
| **ChangeRun** | Contiguous horizontal span of changed cells, used for output optimization. |

---

## Rendering

| Term | Definition |
|------|------------|
| **Presenter** | State-tracked ANSI emitter. Tracks current SGR state, link state, and cursor position to minimize output. |
| **TerminalModel** | Minimal VT emulator used for deterministic testing. Parses ANSI sequences and updates internal state. |
| **Front Buffer** | The last-presented buffer (what the terminal currently shows). |
| **Back Buffer** | The buffer being rendered to (the desired next state). |
| **Scissor** | Clipping rectangle for rendering. Push/pop stack with monotonically decreasing intersection. |
| **Opacity Stack** | Compositing stack where each layer multiplies alpha. Product stays in [0.0, 1.0]. |

---

## Text & Typography

| Term | Definition |
|------|------------|
| **Grapheme** | User-perceived character. May be multiple Unicode codepoints (e.g., emoji with skin tone). |
| **GraphemeId** | 32-bit reference to an interned grapheme. Encodes pool slot (24 bits) + display width (7 bits). |
| **GraphemePool** | Reference-counted interned storage for complex grapheme clusters that don't fit in 4 bytes. |
| **Segment** | Atomic unit of styled text with `Cow<str>` storage and optional style. |
| **Span** | Higher-level wrapper around text + style. Builder for creating styled text units. |
| **Line** | Collection of spans representing a single line of text. |
| **Text** | Multi-line styled text. Collection of Lines with layout information. |
| **Width Cache** | LRU cache for text display width measurements. Default 1000 entries. |

---

## Terminal

| Term | Definition |
|------|------------|
| **TerminalSession** | RAII guard that owns terminal state. Ensures cleanup on exit and panic. |
| **TerminalWriter** | Single gate for all terminal output. Enforces one-writer rule. |
| **TerminalCapabilities** | Detected terminal features: color support, mux detection, synchronized output, etc. |
| **ScreenMode** | Display mode: `Inline` (preserves scrollback) or `AltScreen` (full-screen). |
| **UiAnchor** | Where to pin UI in inline mode: `Top` or `Bottom`. |
| **InlineStrategy** | Inline mode rendering approach: `OverlayRedraw`, `ScrollRegion`, or `Hybrid`. |
| **Raw Mode** | Terminal mode where input is unbuffered and special keys are passed through. |

---

## Escape Sequences

| Term | Definition |
|------|------------|
| **ANSI** | American National Standards Institute. Terminal control standard. |
| **CSI** | Control Sequence Introducer (`ESC [` or `\x1b[`). Starts most control sequences. |
| **SGR** | Select Graphic Rendition. CSI codes for setting text style (bold, color, etc.). |
| **OSC** | Operating System Command (`ESC ]`). Used for setting title, hyperlinks, clipboard. |
| **DCS** | Device Control String. Less common, used for some terminal features. |
| **APC** | Application Program Command. Rarely used in modern terminals. |
| **DEC** | Digital Equipment Corporation. Maker of VT terminals. Some sequences use DEC-specific codes. |
| **OSC 8** | Hyperlink protocol. `ESC ] 8 ; ; URL ST` to start, `ESC ] 8 ; ; ST` to end. |
| **DEC 2026** | Synchronized output mode. Batches terminal updates to reduce flicker. |

---

## Layout

| Term | Definition |
|------|------------|
| **Rect** | Axis-aligned rectangle with x, y, width, height. 0-indexed. |
| **Sides** | Padding/margin values for top, right, bottom, left. CSS-like tuple conversions. |
| **Measurement** | Size hints with min/max constraints for layout negotiation. |
| **Constraint** | Layout sizing rule: Fixed, Percentage, Min, Max, or Ratio. |
| **Flex** | Flexbox-like layout container with direction, gap, and constraints. |

---

## Colors

| Term | Definition |
|------|------------|
| **PackedRgba** | 4-byte packed color with RGB + alpha channel. Supports Porter-Duff compositing. |
| **ColorDepth** | Canonical terminal color capability: `Mono`, `Ansi16`, `Ansi256`, or `TrueColor`. |
| **Color Downgrade** | Pipeline to convert colors to lower capability: TrueColor → 256 → 16 → Mono. |
| **StyleFlags** | 16-bit bitmask for text attributes (bold, italic, underline, etc.). |

---

## Runtime

| Term | Definition |
|------|------------|
| **Program** | Elm-like runtime: Model + update + view loop. |
| **Model** | Application state. Owned by the runtime. |
| **Cmd** | Side effect to execute. Returned by update functions. |
| **Subscription** | Polling-based event source (ticks, input, etc.). |
| **RenderBudget** | Per-frame byte/time budget for output. Controls graceful degradation. |

---

## Unicode

| Term | Definition |
|------|------------|
| **ZWJ** | Zero Width Joiner (U+200D). Connects graphemes into compound characters (e.g., family emoji). |
| **Combining Character** | Character that modifies the preceding base character (accents, diacritics). |
| **Display Width** | Number of terminal columns a character occupies. ASCII = 1, CJK = 2, some emoji = 2. |
| **WTF-8** | Encoding that extends UTF-8 to handle unpaired surrogates. Used in some width testing. |

---

## Compositing

| Term | Definition |
|------|------------|
| **Porter-Duff** | Compositing algebra for alpha blending. Defines how source and destination combine. |
| **Alpha Blending** | Combining colors based on transparency values. |
| **Source Over** | Porter-Duff operation where source is drawn over destination. Most common mode. |

---

## Testing

| Term | Definition |
|------|------------|
| **PTY** | Pseudo-terminal. Used for integration tests that spawn real terminal processes. |
| **Snapshot Test** | Test that compares output against saved "golden" file. |
| **Property Test** | Test that verifies invariants across random inputs (proptest). |
| **Terminal Model Test** | Test that validates presenter output by parsing it with TerminalModel. |

---

## Terminal Multiplexers

| Term | Definition |
|------|------------|
| **Mux** | Terminal multiplexer (tmux, screen, zellij). Adds a layer between application and terminal. |
| **tmux** | Popular terminal multiplexer. Detected via `TMUX` environment variable. |
| **screen** | GNU Screen multiplexer. Detected via `STY` environment variable. |
| **zellij** | Modern terminal multiplexer. Detected via `ZELLIJ` environment variable. |

---

## Safety Concepts

| Term | Definition |
|------|------------|
| **One-Writer Rule** | Only one entity may write to the terminal at a time. Prevents cursor corruption. |
| **Sanitization** | Stripping control sequences from untrusted output. Enabled by default. |
| **Raw Passthrough** | Allowing ANSI sequences through without sanitization. Opt-in, dangerous for untrusted content. |
| **RAII** | Resource Acquisition Is Initialization. Pattern ensuring cleanup via Drop trait. |

---

## Project-Specific

| Term | Definition |
|------|------------|
| **ftui** | FrankenTUI. This project. |
| **Kernel** | Core rendering infrastructure (ftui-render, ftui-core). Must be stable before widgets. |
| **Harness** | Agent harness. Reference application demonstrating inline mode + log streaming. |
| **LogSink** | API for routing in-process logs through FrankenTUI. Implements `std::io::Write`. |
| **PtyCapture** | API for capturing subprocess output via PTY. Preferred for tool output. |

---

## See Also

- [Operational Playbook](operational-playbook.md) - process and shipping order
- [Risk Register](risk-register.md) - failure modes and mitigations
- [State Machines Spec](spec/state-machines.md) - formal invariants
- [ANSI Reference](ansi-reference.md) - escape sequence details
