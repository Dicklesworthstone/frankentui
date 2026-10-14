# ADR-001: Inline Mode Strategy

## Status

ACCEPTED

## Context

FrankenTUI's core promise is stable UI + streaming logs without flicker, cursor drift, or scrollback corruption. Inline mode is where most TUIs fail in practice because:

- Logs write while UI is mid-draw (multiple writers, interleaving)
- Cursor position gets lost or restored incorrectly
- Full-screen clears destroy scrollback
- Terminal multiplexers (tmux/screen/zellij) differ in behavior for cursor save/restore, scroll regions, and synchronized output

This ADR documents the decision from spike bd-10i.1.1.

## Strategies Evaluated

### Strategy A: Scroll-Region Anchoring (DECSTBM)

Uses `CSI t;b r` to set top/bottom scroll margins:
- Log region scrolls within margins
- UI region stays pinned below

**Pros:**
- Reduces redraw work and cursor movement
- More "native" scrolling behavior when it works

**Cons:**
- Muxes (tmux/screen) may handle margins inconsistently
- Must be carefully reset on exit, panic, and mode switches
- Cursor position relative to region can be confusing

### Strategy B: Overlay Redraw

Before each log write and UI present:
1. Save cursor position (DEC ESC7)
2. Move cursor to target region
3. Write content (logs to log area, UI to UI area)
4. Restore cursor position (DEC ESC8)

**Pros:**
- Works in the widest set of environments (baseline correctness)
- No terminal-specific quirks to handle

**Cons:**
- More redraw work
- Requires explicit region policies

### Strategy C: Hybrid (Selected Default)

- **Overlay redraw** is always available as the correctness baseline
- **Scroll-region** is an internal optimization only where proven safe:
  - Not in any terminal multiplexer
  - Scroll region capability detected
  - Synchronized output available (reduces flicker)

## Decision

**Adopt Hybrid strategy (Strategy C) as the default.**

The public API exposes "inline mode" as a policy concept, not the mechanism (DECSTBM). This prevents terminal quirks from leaking into user code.

### Strategy Selection Logic

```rust
fn select_strategy(caps: &TerminalCapabilities) -> InlineStrategy {
    if caps.in_any_mux() {
        // Muxes may not handle scroll regions correctly
        InlineStrategy::OverlayRedraw
    } else if caps.scroll_region && caps.sync_output {
        // Modern terminal with full support
        InlineStrategy::ScrollRegion
    } else if caps.scroll_region {
        // Scroll region available but no sync output
        InlineStrategy::Hybrid
    } else {
        // Fallback to most portable option
        InlineStrategy::OverlayRedraw
    }
}
```

### Fallback Triggers

Overlay redraw is forced when:
1. `TMUX`, `STY`, or `ZELLIJ` environment variable is set
2. Terminal does not report scroll region support
3. User explicitly requests overlay mode

## Cursor Save/Restore

**Use DEC sequences (ESC7/ESC8), not CSI s/u.**

Rationale from spike investigation:
- DEC sequences are more portable across terminals
- CSI s/u conflict with scroll region operations in some terminals
- Legacy opentui_rust uses DEC sequences successfully

## Cleanup Invariants

On normal exit AND panic, the following must be restored:
1. Scroll region reset to full screen (`CSI r`)
2. Cursor restored (`ESC 8`)
3. Synchronized output ended (`CSI ? 2026 l`)
4. Raw mode exited (handled by RawModeGuard)

Implementation uses RAII `Drop` trait to guarantee cleanup.

## Synchronized Output

Use DEC mode 2026 (`CSI ? 2026 h/l`) when available to:
- Batch terminal updates atomically
- Reduce flicker during UI redraw
- Detection via `TerminalCapabilities::sync_output`

## Consequences

### Positive
- Correctness guaranteed across all terminals via overlay baseline
- Optimized path for modern terminals without muxes
- No terminal quirks exposed in public API
- Cleanup guaranteed even on panic

### Negative
- Overlay mode has more redraw overhead
- Users cannot force scroll-region in muxes (by design)
- Testing requires PTY harness for full validation

## Test Plan

1. **PTY Integration Tests**
   - Cursor restored after each frame present
   - Terminal modes restored on normal exit
   - Terminal modes restored on panic (simulated)
   - No full-screen clears in inline mode

2. **Terminal Compatibility Matrix**
   - Plain terminal (no mux): scroll-region optimization
   - tmux: overlay redraw only
   - screen: overlay redraw only
   - Kitty/WezTerm/Alacritty/Ghostty: scroll-region optimization

3. **Sustained Output Scenario**
   - Continuous log stream + periodic UI redraw + resize events
   - Verify no cursor drift over extended operation

## Amendment (2026-10-13): log lines must reach scrollback in every strategy (G48)

Bead `bd-g00-root-epic-ewths.47` (children `.47.1`–`.47.6`).

### Defect

Strategy B as shipped through v0.9.0 never scrolled: each log line was
erased-and-rewritten on one fixed row (`ESC 7`, `CUP`, `EL`, text, `ESC 8`),
so only the newest line was ever visible and nothing reached scrollback.
Overlay is what every multiplexer identity selects and what a failed DECSTBM
self-test falls back to, so inside tmux `agent_shell -- seq 1 500` left 0 of
500 lines in history while plain `seq` left 500. Tests asserted byte patterns
("no LF") and so pinned the loss as correct; `bd-th0p6` had diagnosed it in
August and was closed without a fix.

### Measurements (2026-10-13, this host)

`tests/e2e/scripts/test_inline_scrollback_mux.py` runs `agent_shell
--exit-when-child-exits -- seq 1 N` inside real multiplexers (private
sockets/sessions) next to a control run of the same child, and reads history
back. N = 10,000:

| environment | control `seq` | pre-fix `befbd247` | after this amendment |
|---|---|---|---|
| tmux 3.6, tmux identity (overlay) | 10000/10000 | 0/10000 | 10000/10000 |
| tmux 3.6, no identity, DECSTBM forced | 10000/10000 | 9999/10000 (exit overwrote a row) | 10000/10000 |
| tmux 3.6, no identity, overlay forced | 10000/10000 | 0/10000 | 10000/10000 |
| GNU screen 4.09 (overlay) | 10000/10000 | 0/10000 | 10000/10000 |
| zellij 0.43.1 (overlay) | 10000/10000 | 0/10000 | 10000/10000 |

After the amendment the UI's status row appears exactly once (its final frame;
no UI row in history), the shell's next output starts on a row of its own, and
the final frame shows the final line count. In-process, the same properties
are checked against an independent emulator (the `vt100` crate) by
`crates/ftui-runtime/tests/e2e_inline_mode_scrollback.rs` (11 of 14 cases fail
on `befbd247`).

Scroll-region scrollback is terminal-dependent: xterm keeps rows that leave a
region whose top margin is row 1, tmux 3.6 does (measured above), Alacritty
does since its 2024 change that stopped regions *not* starting at the top
from feeding history; the `vt100` crate keeps only full-screen scrolls.
**Unmeasured here** (no host): kitty, WezTerm (incl. its mux), GNOME VTE,
iTerm2, Windows Terminal, nested tmux, tmux versions before 3.6.

### Decision

1. **Overlay transport = whole-screen scroll stream.** Bottom-anchored UI:
   complete log lines are batched and streamed at the next present, explicit
   flush or teardown (or early at 256 lines / 64 KiB): erase the UI rows
   (`CUP T;1`, `ED 0`), write the lines from row `T` with `CR LF`, then `u-1`
   more `CR LF`, and redraw the UI. The terminal's own LF at the bottom row
   scrolls the whole screen, which every terminal keeps in scrollback, and the
   accounting needs no width arithmetic (autowrap breaks long lines). Cost: one
   UI redraw per batch.
2. **The multiplexer veto stays.** With (1) logs reach history in muxes
   without DECSTBM; lifting the veto for tmux ≥ 3.6 would save the redraw but
   rests on one measured version. Revisit with the unmeasured list above.
3. **Room before the first frame and before growth.** The first inline output
   emits `u-1` `CR LF` from the shell's cursor so the UI never draws over the
   shell's last lines; an inline-auto UI that grows scrolls by the growth first.
   Before any frame is displayed the writer remembers the row after the last
   streamed line, and neither a later stream nor the first frame starts above
   it (a property test found the stream erasing a line after a pending
   inline-auto height grew between two flushes).
4. **Teardown contract.** The final frame stays on screen; the cursor is
   parked at the start of an empty row below it (after the region reset, which
   homes the cursor on real terminals — the pre-fix region path let the shell
   overwrite the top row). On panic the log flush is skipped, parking is not.
   This supersedes "Cursor restored (`ESC 8`)" under Cleanup Invariants.
5. **Final frame.** A quit from an event/subscription/task update previously
   left the previous frame on screen; inline programs now draw once more if the
   model changed, with the frame budget reset (it may be at SkipFrame after an
   output storm).
6. **Top-anchored UI** keeps the single-row overwrite: a whole-screen scroll
   would push UI rows into history, and a region not starting at row 1 never
   feeds it. Documented as a limitation.
7. **Resizes (found by the G42 flagship journey, `.42.7`).** A terminal reads
   the writer's bytes at whatever size it has when they arrive, which can be
   after a resize the writer has not seen. With absolute `CUP` rows, one tmux
   resize during streaming lost 105 of 100,000 lines and a 50-resize storm
   about 1,900: on a grow the `ED 0` landed on rows tmux had pulled back from
   history, on a shrink below the UI, which then scrolled into history.
   Telling the writer the size sooner did not help (measured, reverted). So
   the bottom-anchored overlay UI is addressed from the terminal's last row
   (`CSI 9999;colH` then `CUU`), which a multiplexer keeps at the bottom.
   tmux also deletes the rows *below the cursor* first when it shrinks a pane,
   so a visible input cursor above the UI's last row costs the UI that row;
   before each erase or frame the writer re-anchors from the cursor's known
   row in the UI with one `CR LF` per row below it (these scroll the missing
   rows back where tmux deleted them). That row survives `set_size`. Result,
   tmux 3.6, 100,000 lines: one resize and both 50-resize storms deliver every
   line once, in order. When the width shrinks, tmux reflows the UI's
   full-width rows and part of a frame can overflow into history (35 fragment
   rows in the width storm; none with height-only resizes): a known
   limitation, `bd-g00-root-epic-ewths.42.8`. DECSTBM strategies keep
   absolute rows (cursor-up there is bounded by the region's margins).

## References

- Spike implementation: `crates/ftui-core/src/inline_mode.rs`
- Terminal capabilities: `crates/ftui-core/src/terminal_capabilities.rs`
- Legacy patterns: `legacy_reference_library_code/opentui_rust/src/terminal/`
