# Suspend and resume (SIGTSTP / SIGCONT) — design

Status: **implemented for the native TTY backend** (`bd-d4dtr`). Written for
`bd-g00-root-epic-ewths.37.1` as a design; **§11 records where the build
departs from it**, and wins where the two disagree. The crossterm backend
does not support suspend yet: under it the stop signals keep their default
action.

Today `kill -TSTP` on an inline session leaves the shell in raw mode: the
process stops with the terminal still in the state the app configured, so the
recovered shell has no echo and no line discipline. Ctrl-Z does not even reach
the kernel as a signal, because raw mode clears `ISIG`.

Every crate here is `#![forbid(unsafe_code)]`, so the whole design has to be
expressible through safe APIs. It is — see **Stopping the process**, which is
simpler than expected.

---

## 1. Scope

- **Unix only** (`cfg(unix)`). Windows has no job control; the switch compiles
  to a no-op there.
- **Both backends**: `ftui-tty` (native termios via `RawModeGuard`) and
  crossterm (`TerminalSession`).
- **Both modes**: inline and alt-screen. They differ only in what the teardown
  and re-arm sequences contain.
- **Two entry paths**, which are *not* the same and are the most commonly
  missed part of this problem:
  - `kill -TSTP <pid>` from outside arrives as a real signal.
  - **Ctrl-Z from the keyboard does not.** Raw mode clears `ISIG`, so the
    terminal driver never converts `^Z` into SIGTSTP; it arrives as
    `Event::Key(Char('z'), CTRL)`. The runtime must recognise it and request
    its own suspend.
- `SIGTTIN` / `SIGTTOU` are handled identically to SIGTSTP (all three are
  `Stop`-class; see §4).

---

## 2. State machine

```
Running ──(Ctrl-Z key | SIGTSTP/SIGTTIN/SIGTTOU)──> SuspendRequested{source}
SuspendRequested ──(teardown done, process stopped)──> Suspended
Suspended ──(SIGCONT)──> Resuming
Resuming ──(re-arm done, repaint forced)──> Running
```

Invariants:

1. **No terminal I/O in a signal handler.** Handlers set an atomic and wake the
   loop; every escape sequence is written from the main loop.
2. **Exactly one teardown and one re-arm per cycle.** A second SIGTSTP arriving
   during `SuspendRequested` coalesces into the first (§9).
3. **Termination outranks suspension.** If a termination signal is pending when
   the process resumes — e.g. SIGTERM delivered while stopped — the normal exit
   path runs instead of `Resuming`. `Program::pending_signal` is checked before
   `pending_suspend`.
4. `Suspended` executes no Rust code. It is the window between the stop and the
   kernel resuming the thread inside the same call.

---

## 3. Handler design

`Program` already owns `pending_signal: Arc<AtomicI32>` (`program.rs:5242`) for
termination. Suspension mirrors it:

```rust
pending_suspend: Arc<AtomicI32>,   // the signal number, or 0
resume_flag:     Arc<AtomicBool>,  // set by SIGCONT
```

Registration, all safe API from `signal-hook` 0.3.18:

```rust
use signal_hook::consts::{SIGCONT, SIGTSTP, SIGTTIN, SIGTTOU};

for sig in [SIGTSTP, SIGTTIN, SIGTTOU] {
    signal_hook::flag::register_usize(sig, Arc::clone(&pending_suspend_usize), sig as usize)?;
}
signal_hook::flag::register(SIGCONT, Arc::clone(&resume_flag))?;
```

`flag::register_usize` stores *which* signal arrived, which the evidence row
needs; `flag::register` is enough for SIGCONT because only its arrival matters.
Both only touch an atomic, so they are async-signal-safe by construction.

The existing signal thread (`signal_hook::iterator::Signals`) is the
alternative and is equally safe; prefer it only if the implementation already
needs the thread for something else. The flag route has fewer moving parts and
no thread to pause across the stop.

Waking the loop: the poll must not sit in a 100 ms timeout after a suspend
request. Reuse whatever wake channel the input thread already has; if there is
none, the smallest addition is a self-pipe or an `mpsc` sentinel the poll
selects on. **Do not** rely on the signal interrupting `poll` with `EINTR` —
`signal-hook` installs handlers with `SA_RESTART`.

---

## 4. Stopping the process

This is the part the bead flagged as non-trivial ("signal-hook disposition
juggling"). **It is not, and the reason is worth stating precisely**, because
the obvious mental model is wrong.

`signal_hook::low_level::emulate_default_handler(SIGTSTP)` does **not** reset
the disposition and re-raise SIGTSTP. For a `Stop`-class signal it does exactly
one thing:

```rust
DefaultKind::Stop => low_level::raise(SIGSTOP),
```

(`signal-hook-0.3.18/src/low_level/signal_details.rs:181`, with
`s!(SIGTSTP, Stop)`, `s!(SIGTTIN, Stop)`, `s!(SIGTTOU, Stop)` at `:71-73`.)

So there is no disposition juggling to get wrong, and no race: the
reset-then-reraise dance in that function is on the `Term` path only, for
signals that end the process. `SIGSTOP` cannot be caught or blocked, so the
kernel stops the process unconditionally, and the shell's `waitpid` reports
`WIFSTOPPED` exactly as it would for SIGTSTP. Job control works.

**Use `emulate_default_handler(signal)` and pass the signal that arrived.**
Do not hand-roll unregister/raise/re-register; it is more code, it is racy in a
multi-threaded program, and it buys nothing here.

Two consequences to write down because they will surprise someone:

- The process stops on **SIGSTOP**, not SIGTSTP. Anything inspecting the stop
  signal (a supervisor, a test harness reading `WSTOPSIG`) sees `SIGSTOP`.
- `raise` is thread-directed. On a multi-threaded runtime the *calling thread*
  is the one that requests the stop, but SIGSTOP stops **all** threads in the
  process, so the input thread is stopped too whether or not it was paused
  first. Pausing it is still worth doing (§5a) so it does not read a
  half-restored terminal on the way down.

---

## 5. Suspend sequence (main loop)

On observing `pending_suspend != 0` (or a Ctrl-Z key with job control on):

a. **Pause input polling.** Mark the input thread paused so it stops touching
   the fd. It will be stopped by the kernel anyway; this only avoids a read
   racing the termios change.
b. **`session_teardown::suspend(&mut session)`** — the reversible subset of the
   existing teardown in `ftui-core::session_teardown`, in this order:
   1. kitty keyboard pop
   2. mouse off (`?1000l` `?1002l` `?1006l`)
   3. bracketed paste off (`?2004l`)
   4. focus reporting off
   5. cursor show (`?25h`)
   6. **inline**: reset DECSTBM (`\x1b[r`), then place the cursor on the row
      below the UI region so the shell prompt lands under the app's output
      rather than on top of it.
      **alt-screen**: leave (`?1049l`), which restores the primary screen.
   7. flush
   8. termios back to cooked, through whichever safe API that backend uses
      (`RawModeGuard` for `ftui-tty`, crossterm's `disable_raw_mode`)
c. Emit evidence: `suspend { signal, mode, cols, rows, ts }`.
d. Clear `pending_suspend`.
e. `signal_hook::low_level::emulate_default_handler(signal)` — §4. Execution
   blocks here.
f. Control returns on SIGCONT, in the same call, on the same thread.

The order in (b) is the existing teardown order and is not arbitrary: kitty pop
before mouse off because the pop is latched once; cursor show before termios
restore so the sequence is written while the terminal still interprets it;
termios last because after it the app no longer owns the terminal.

---

## 6. Resume sequence

g. Read and clear `resume_flag`.
h. Re-arm in **startup** order, the mirror of (b): raw mode on → alt screen
   (`?1049h`) if that mode → kitty push → mouse on → paste on → focus on →
   cursor hide → inline: re-query size and re-establish DECSTBM.
i. **Force a full repaint.** The terminal's contents are not ours any more —
   the user may have run anything at the shell. Invalidate the writer's diff
   baseline the same way a resize does (`TerminalWriter`'s `ui_anchor`,
   `scroll_region_active` and `last_inline_region` — `terminal_writer.rs:572`,
   `:599`, `:606` as of 2026-09-19) and mark the model dirty. A diff against a
   stale baseline is the classic way this bug reappears as corrupted output
   rather than as a crash.
j. If the size changed while suspended, deliver a `Resize` event **before** the
   repaint so layout is computed once at the new size.
k. Emit evidence: `resume { ts, cols, rows, size_changed }`.
l. Resume input polling.

---

## 7. Ctrl-Z as a key

With `ISIG` off, `^Z` is `Event::Key(Char('z'), CTRL)`. The runtime maps it to a
suspend request **before** dispatching to the model, unless the application has
bound `Ctrl-Z` itself — the G14 keymap priority rules decide, so an editor that
wants Ctrl-Z for undo keeps it.

`ProgramConfig::with_job_control(bool)`: default `true` for interactive
constructors, `false` for headless and simulator. With it off, `^Z` is an
ordinary key event and external SIGTSTP is not registered.

---

## 8. Interaction with existing work

- **Per-program signal state.** `pending_suspend` lives on `Program` beside
  `pending_signal`. Only interactive constructors install OS handlers; headless
  ones leave both atomics inert, which is what makes the injection seam below
  safe to expose.
- **`session_teardown`.** Gains `suspend()` and `resume()`. `teardown()` stays
  the terminal path; `suspend()` is its reversible subset. The kitty pop-once
  latch must **reset** on resume, or the second suspend of a session skips the
  pop and leaves the keyboard protocol pushed.

---

## 9. Failure modes

| Situation | Behaviour |
|---|---|
| SIGTSTP during teardown | Coalesce — `pending_suspend` is already set; do not start a second teardown. |
| SIGCONT with no prior stop (`kill -CONT`) | No-op. `resume_flag` set while `Running` is cleared without re-arming. |
| `tcsetattr` fails on resume (terminal gone) | Normal error exit path, not a panic. The session is unusable; report it. |
| Termination signal pending at resume | Exit path wins (§2 invariant 3). |
| Size changed while suspended | `Resize` first, then repaint (§6j). |
| Suspend while already `Suspended` | Impossible — no Rust code runs in that state. |

---

## 10. Test plan

**Unit (state machine).** Table-driven transitions, including the coalescing
and precedence rules in §2 and §9. No terminal needed.

**Harness (golden byte sequences).** A headless writer records the bytes the
suspend sequence emits and the bytes the resume sequence emits; assert both
against goldens, per mode. This is what pins the *order* in §5b and §6h, which
is the part that breaks silently — a wrong order still produces every sequence,
so only an ordered assertion catches it.

**Injection seam.** `inject_suspend_signal(sig)` on `Program`, test-only, sets
`pending_suspend` exactly as the handler would. Tests must not actually raise
SIGTSTP: a stopped test process hangs the suite with no output.

**PTY (the real thing).** The one case the above cannot cover is whether the
*shell* is usable after the stop. Drive a real PTY, send `kill -TSTP`, and
assert the terminal is back in cooked mode (echo on, `ISIG` on) while stopped;
then `kill -CONT` and assert raw mode is re-armed and a full repaint followed.
`tests/e2e/scripts/test_cleanup.sh` already captures termios state this way and
is the closest existing pattern.

---

## Open questions for the implementer

1. **Does the input thread need explicit pausing at all?** SIGSTOP stops it
   regardless (§4). The argument for pausing is the read/termios race on the
   way down; measure whether it is reachable before adding the machinery.
2. **Which wake mechanism.** Depends on what the poll loop already selects on
   when this is implemented; `SA_RESTART` rules out relying on `EINTR`.
3. **Inline cursor placement on suspend** (§5b.6) needs the row below the UI
   region, which is `TerminalWriter` state. Confirm it is still accurate after
   a partial frame.

---

## 11. As built (bd-d4dtr)

Where each piece lives:

| Piece | Where |
|---|---|
| Stop-signal handlers, claims, `stop_process` | `ftui_core::job_control` |
| Hooks | `BackendEventSource::{supports_suspend, suspend, resume}`, `BackendPresenter::suspend` |
| Terminal hand-back and re-arm | `TtyBackend::{suspend_session, resume_session}`, `RawModeGuard::{restore_original, reenter_raw}` |
| Presenter state (sync block, scroll region, cursor) | `TerminalWriter::release_for_suspend`, the drop-time cleanup without the trace finish |
| Sequence, Ctrl-Z, evidence | `Program::service_suspend`, `ProgramConfig::{job_control, ctrl_z_suspends}` |

Departures from the design above, each for a measured reason:

1. **Ctrl-Z is opt-in** (`ProgramConfig::with_ctrl_z_suspend`), not on by
   default as §7 proposed. Ctrl-Z is undo in `TextArea` and in the showcase's
   app, forms and advanced text editor, and nothing tells the runtime whether
   a model binds it, so intercepting it by default would have broken undo.
   External stop signals *are* handled by default (`job_control: true`):
   before this they stranded a raw terminal, so handling them only fixes a bug.
2. **No SIGCONT handler.** Resume runs when `stop_process` returns, which is
   exactly when the process is continued, so `resume_flag` has nothing to
   add. A stray `kill -CONT` keeps its default (ignore) action. One gap
   follows: `kill -TSTP` then `kill -CONT` sent before the loop serves the
   first leaves the process stopped until a second CONT, because the
   handler cannot tell the order the two arrived in.
3. **Handlers are process-global and never uninstalled,** because
   signal-hook cannot remove one. Each is paired with
   `register_conditional_default`, which performs the default stop while no
   `JobControlClaim` is held. Without that, a process whose program had
   exited could never be suspended again.
4. **No dedicated wake channel.** A stop signal is served within one poll
   interval (100 ms by default), the same latency termination signals already
   have: `poll_tty` returns on `EINTR` and the loop checks both flags.
5. **Inline cursor** is left where the drop-time cleanup leaves it (the
   restored cursor), not moved below the UI region: suspend and exit put the
   shell prompt in the same place, and open question 3 stays open.
6. **The input thread is not paused** (open question 1): the TTY backend
   reads on the loop thread, so nothing races the termios change.

Evidence rows `suspend` and `resume` are specified in
`docs/spec/telemetry-events.md`. Tests: ordered-sequence unit tests in
`ftui-runtime/src/program.rs` (`stop_request_releases_stops_and_restores_in_order`
and neighbours), and a real-PTY test that sends `kill -TSTP`, reads the PTY's
termios while the process is stopped, sends `kill -CONT` and checks raw mode
and a repaint, in alt-screen and inline mode
(`ftui-harness/tests/pty_terminal_lifecycle.rs`,
`pty_stop_signal_hands_back_the_terminal_and_continue_takes_it_again_*`).
