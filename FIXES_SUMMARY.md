# Fixes Summary - Session 2026-02-01 (Part 13)

## 34. PTY Capture Backpressure
**File:** `crates/ftui-extras/src/pty_capture.rs`
**Issue:** `PtyCapture` used an unbounded `mpsc::channel` to buffer output from the child process. If the reader (main loop) was slow or blocked, the channel could grow indefinitely, leading to OOM.
**Fix:** Switched to `mpsc::sync_channel(1024)` (approx 8MB buffer). This introduces backpressure: if the buffer fills, the reader thread blocks, which in turn stops reading from the PTY, causing the PTY buffer to fill and eventually blocking the child process's writes. This safely handles high-throughput subprocesses.

## 35. Program Thread Safety
**File:** `crates/ftui-runtime/src/program.rs`
**Issue:** `Program` generic `W` did not explicitly require `Send`. While `Stdout` is `Send`, custom writers might not be. This limits `Program`'s usability in threaded contexts (e.g. inside `tokio::spawn` or a dedicated render thread).
**Fix:** Added `Send` bound to `W: Write + Send` in `Program` struct definition.

## 36. Link Registry Integration
**Files:** `crates/ftui-render/src/frame.rs`, `crates/ftui-runtime/src/program.rs`, `crates/ftui-text/src/text.rs`
**Issue:** Widgets had no way to register OSC 8 hyperlinks because `Frame` didn't expose the `LinkRegistry`. `Style` also lacked a way to carry URL information.
**Fix:** 
    - Added `link: Option<Cow<'a, str>>` to `Span`.
    - Added `links: Option<&'a mut LinkRegistry>` to `Frame`.
    - Updated `Program::render_frame` to pass the registry to the frame.
    - Updated `TerminalWriter::render_resize_placeholder` to handle the new frame setup.
    - Note: `draw_text_span` logic update is pending in `ftui-widgets`, but the infrastructure is now in place.

## 37. Rendering Safety
**File:** `crates/ftui-render/src/sanitize.rs` (Previous fix confirmed)
**Issue:** Log swallowing via malformed escape sequences.
**Fix:** Verified that the new `skip_escape_sequence` logic is robust against invalid control characters inside sequences.