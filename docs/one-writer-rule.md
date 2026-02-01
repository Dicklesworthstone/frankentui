# One-Writer Rule: Guidance and Routing Patterns

This guide explains FrankenTUI's **one-writer rule** and shows you how to safely output content while maintaining terminal state integrity.

> **TL;DR**: When FrankenTUI is active, all terminal output must flow through FrankenTUI's APIs. Direct `println!()` or raw stdout writes will corrupt your terminal display.

## What is the One-Writer Rule?

The one-writer rule states that **only one entity may write to the terminal at a time**. In FrankenTUI, this is the `TerminalWriter` owned by your `TerminalSession`.

### Why It Matters

Terminals are shared mutable state. Concurrent or interleaved writes cause:

- **Cursor drift**: The cursor ends up in unexpected positions
- **Partial sequences**: Half-written escape codes produce garbage
- **Interleaved output**: UI and logs mix unpredictably
- **State corruption**: Mode changes (raw mode, alt screen) can be lost

FrankenTUI's inline mode is especially sensitive because it overlays UI on your scrollback.

### What Counts as Terminal Output?

Any bytes written to stdout or stderr that reach the terminal:

| Type | Examples | Status |
|------|----------|--------|
| **Must route through FrankenTUI** | Application logs, tool output, progress updates | |
| **Automatically handled** | FrankenTUI UI rendering, frame presentation | |
| **Undefined behavior** | `println!()`, `eprintln!()`, raw `std::io::stdout().write()` | |

## Supported Routing Patterns

FrankenTUI provides three patterns for safely routing output. Choose based on your use case.

### Pattern A: LogSink (In-Process Logs)

For application logs and messages you control:

```rust
use ftui::LogSink;

fn main() -> Result<()> {
    let app = App::new()?;

    // Get a log sink from the app
    let log = app.log_sink();

    // Route your logs through the sink
    writeln!(log, "Starting process...")?;
    writeln!(log, "Loaded {} items", count)?;

    // Logs appear in your UI's log viewport
    app.run()
}
```

**Key points:**
- `LogSink` implements `std::io::Write`
- All output is automatically sanitized (see [ADR-006](adr/ADR-006-untrusted-output-policy.md))
- Logs are buffered and rendered in your designated log area
- Thread-safe: multiple threads can write to cloned sinks

### Pattern B: PTY Capture (Subprocess Output)

For running external tools and capturing their output:

```rust
use ftui::pty::PtyCapture;

fn main() -> Result<()> {
    let app = App::new()?;

    // Spawn a subprocess with PTY capture
    let capture = PtyCapture::spawn(&["cargo", "build"])?;

    // Forward output to your log viewport
    app.attach_pty(capture);

    app.run()
}
```

**Key points:**
- Subprocess runs in a pseudo-terminal
- Output is captured and routed through FrankenTUI
- ANSI sequences from trusted tools can be preserved (opt-in)
- Use for: build tools, test runners, shell commands

**Example: Streaming Tool Output**

```rust
use ftui::{App, pty::PtyCapture};

fn stream_cargo_build(app: &mut App) -> Result<()> {
    // Capture cargo build with color output
    let mut pty = PtyCapture::spawn(&["cargo", "build", "--color=always"])?;

    // Allow ANSI colors (trusted tool)
    pty.set_passthrough_sgr(true);

    // Stream to log viewport
    while let Some(chunk) = pty.read_chunk()? {
        app.log_viewport().append_raw(chunk);
    }

    Ok(())
}
```

### Pattern C: Stdio Capture (Best-Effort)

For catching accidental writes from libraries:

```rust
use ftui::StdioCapture;

fn main() -> Result<()> {
    // Enable stdio capture (must be done before App::new)
    let _guard = StdioCapture::enable()?;

    let app = App::new()?;

    // Now even println!() from third-party code is captured
    // (best-effort, not 100% reliable)

    app.run()
}
```

**Important caveats:**
- This is **feature-gated** (`features = ["stdio-capture"]`)
- Not all output can be captured (native code, FFI)
- Some writes may still slip through
- Adds runtime overhead
- Use as a safety net, not primary strategy

## Safety Notes

### Sanitize by Default

FrankenTUI sanitizes untrusted output by default (see [ADR-006](adr/ADR-006-untrusted-output-policy.md)):

- Control sequences (CSI, OSC, DCS, APC) are stripped
- C0 controls except TAB, LF, CR are removed
- Prevents escape sequence injection attacks

```rust
// Safe: content is sanitized
log.write_all(user_input.as_bytes())?;

// Dangerous: raw passthrough, use only for trusted sources
log.write_raw(trusted_tool_output)?;
```

### When Raw Passthrough is Appropriate

Use raw passthrough **only** for:

- Output from tools you control and trust
- Test harnesses where you need ANSI colors
- Development/debugging with known-safe content

**Never** use raw passthrough for:

- User-provided content
- LLM-generated text
- Network-sourced data
- Untrusted subprocess output

### Terminal Multiplexer Notes

When running under tmux, screen, or zellij:

1. **Passthrough mode** may be needed for some features
2. **Clipboard (OSC 52)** requires `set-clipboard on` in tmux
3. **Bracketed paste** works with modern mux versions
4. FrankenTUI auto-detects mux environment and adjusts

Example tmux configuration:
```
set -g set-clipboard on
set -g allow-passthrough on
```

## Undefined Behavior

The following actions are **undefined behavior** when FrankenTUI is active:

| Action | What Happens |
|--------|--------------|
| `println!()` | Output interleaves with UI, cursor corruption |
| `eprintln!()` | Same as above |
| Raw `stdout.write()` | Terminal state becomes unpredictable |
| `std::process::Command` without PTY | Output bypasses FrankenTUI |
| Third-party logging to stdout | May corrupt display |

### Debugging Output Issues

If you see corrupted output:

1. **Check for println!**: Search your code and dependencies
2. **Check subprocess spawning**: Use PTY capture, not raw Command
3. **Check tracing/log crates**: Route to LogSink, not stdout
4. **Enable stdio capture**: Catches most accidental writes

## Quick Reference

```rust
// DO: Use LogSink for your logs
let log = app.log_sink();
writeln!(log, "status: {}", status)?;

// DO: Use PTY capture for subprocesses
let pty = PtyCapture::spawn(&["my-tool"])?;
app.attach_pty(pty);

// DON'T: Direct terminal writes
println!("status: {}", status);  // BAD!

// DON'T: Raw subprocess without PTY
Command::new("my-tool").spawn()?;  // BAD!
```

## Related Documentation

- [ADR-005: One-Writer Rule Enforcement](adr/ADR-005-one-writer-rule.md) - Architectural decision
- [ADR-006: Untrusted Output Policy](adr/ADR-006-untrusted-output-policy.md) - Sanitization details
- [Inline Mode Guide](inline-mode.md) - How inline mode works
