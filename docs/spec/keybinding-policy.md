# Keybinding Policy Specification

This document specifies the behavior of key bindings for input clearing, task
cancellation, and overlay/mode toggling in FrankenTUI applications.

See Bead: bd-2vne.1.

---

## 1) Goals

- Define unambiguous behavior for common interrupt/cancel keys across all states.
- Support single-hand use and muscle memory from shell/CLI conventions.
- Enable multi-key sequences (e.g., Esc Esc) without blocking single-key events.
- Remain correct on terminals where Esc timing is ambiguous.

## 2) Non-Goals

- Per-key rebinding (handled by a separate keybinding config layer).
- Vi/Emacs mode switching (out of scope for core policy).
- Terminal-specific workarounds beyond conservative defaults.

---

## 3) State Variables

The keybinding policy depends on these runtime state flags:

| Flag | Type | Description |
|------|------|-------------|
| `input_nonempty` | bool | True if the text input buffer contains characters |
| `task_running` | bool | True if a background task/command is executing |
| `modal_open` | bool | True if a modal dialog or overlay is visible |
| `view_overlay` | bool | True if a secondary view (tree, debug, HUD) is active |

These flags are queried at the moment a key event is resolved to an action.

---

## 4) Key Definitions

### 4.1 Primary Keys

| Key | Raw Code | Notes |
|-----|----------|-------|
| Ctrl+C | `KeyCode::Char('c')` + `Modifiers::CTRL` | SIGINT equivalent |
| Ctrl+D | `KeyCode::Char('d')` + `Modifiers::CTRL` | EOF / soft quit |
| Ctrl+Q | `KeyCode::Char('q')` + `Modifiers::CTRL` | Hard quit |
| Esc | `KeyCode::Escape` | Cancel / dismiss |
| Esc Esc | Two `Escape` within timeout | Toggle overlay view |

### 4.2 Timing Constants

| Constant | Default | Range | Description |
|----------|---------|-------|-------------|
| `ESC_SEQ_TIMEOUT_MS` | 250 | 150–400 | Max gap between Esc presses for double-Esc |
| `ESC_DEBOUNCE_MS` | 50 | 0–100 | Minimum wait before treating Esc as single |

Rationale:
- 250ms is the median human double-tap interval for experienced users.
- 50ms debounce avoids false triggers from terminal escape sequence parsing.

---

## 5) State Machine

```
                                    ┌─────────────────────────────────────┐
                                    │                                     │
                                    ▼                                     │
┌──────────┐   Esc   ┌────────────────────┐  timeout    ┌─────────┐      │
│  Idle    │───────▶│  AwaitingSecondEsc  │────────────▶│ Emit(Esc)│      │
└──────────┘         └────────────────────┘              └─────────┘      │
     ▲                        │                                           │
     │                        │ Esc (within timeout)                      │
     │                        ▼                                           │
     │               ┌─────────────────┐                                  │
     │               │ Emit(EscEsc)    │──────────────────────────────────┘
     │               └─────────────────┘
     │
     │  other key
     └───────────────────────────────────────────────────────────────────
```

State transitions:
1. **Idle**: Default state, waiting for input.
2. **AwaitingSecondEsc**: After first Esc, wait up to `ESC_SEQ_TIMEOUT_MS`.
3. **Emit(Esc)**: Timeout expired, emit single Esc action.
4. **Emit(EscEsc)**: Second Esc received in time, emit double-Esc action.

Implementation notes:
- State is per-input-stream, not global.
- Other keys during AwaitingSecondEsc emit the pending Esc first, then process.
- Release events are ignored for sequence detection (only Press/Repeat).

---

## 6) Action Resolution

Actions are resolved in priority order. The first matching rule fires.

### 6.1 Priority Table

| Priority | Condition | Key | Action |
|----------|-----------|-----|--------|
| 1 | `modal_open` | Esc | `DismissModal` |
| 2 | `modal_open` | Ctrl+C | `DismissModal` |
| 3 | `input_nonempty` | Ctrl+C | `ClearInput` |
| 4 | `task_running` | Ctrl+C | `CancelTask` |
| 5 | `!input_nonempty && !task_running` | Ctrl+C | `Quit` (configurable) |
| 6 | `view_overlay` | Esc | `CloseOverlay` |
| 7 | `input_nonempty` | Esc | `ClearInput` |
| 8 | `task_running` | Esc | `CancelTask` |
| 9 | always | Esc Esc | `ToggleTreeView` |
| 10 | always | Ctrl+D | `SoftQuit` |
| 11 | always | Ctrl+Q | `HardQuit` |

### 6.2 Action Definitions

| Action | Behavior |
|--------|----------|
| `ClearInput` | Empty the input buffer, keep cursor at start |
| `CancelTask` | Send cancel signal to running task, update status |
| `DismissModal` | Close topmost modal, return focus to parent |
| `CloseOverlay` | Deactivate view overlay (tree view, debug HUD) |
| `ToggleTreeView` | Toggle the tree/file view overlay |
| `Quit` | Clean exit via `Cmd::Quit` |
| `SoftQuit` | Quit if idle, otherwise cancel current operation |
| `HardQuit` | Immediate quit (bypass confirmation if any) |
| `PassThrough` | Forward event to focused widget/input |

### 6.3 Ctrl+C Empty-Idle Behavior

When `!input_nonempty && !task_running && !modal_open`:

| Config Value | Behavior |
|--------------|----------|
| `"quit"` (default) | Exit the application |
| `"noop"` | Do nothing |
| `"bell"` | Emit terminal bell (BEL) |

This is configurable via `FTUI_CTRL_C_IDLE_ACTION` or runtime config.

---

## 7) Conflict Matrix

### 7.1 Terminal Escape Ambiguity

Some terminals emit raw `ESC` (0x1B) as part of escape sequences. The input
parser (`ftui-core/src/input_parser.rs`) handles this by buffering and waiting
for sequence completion. By the time events reach the keybinding layer, `Esc`
means a standalone Escape key, not a sequence prefix.

### 7.2 Multiplexer Interactions

| Environment | Issue | Mitigation |
|-------------|-------|------------|
| tmux | Esc delay (`escape-time`) | User should set `escape-time 0` |
| screen | Esc delay | Conservative timeout in ftui |
| zellij | Generally clean passthrough | No special handling needed |
| ssh | Network latency | Longer timeout may help |

### 7.3 Terminal-Specific Notes

| Terminal | Ctrl+C | Esc | Notes |
|----------|--------|-----|-------|
| Kitty | Clean | Clean | Full support |
| WezTerm | Clean | Clean | Full support |
| Alacritty | Clean | Clean | Full support |
| iTerm2 | Clean | Clean | Full support |
| GNOME Terminal | Clean | Clean | Full support |
| Windows Terminal | Clean | Minor delay | Esc may have ~50ms delay |
| conhost | May send SIGINT | Delayed | Legacy, best-effort only |

---

## 8) Accessibility Considerations

### 8.1 Single-Hand Use

- All primary interrupt keys (Ctrl+C, Ctrl+Q, Esc) are reachable with left hand.
- Esc Esc avoids modifier chords for overlay toggle.
- Consider future support for sticky modifiers (not in v1 scope).

### 8.2 Muscle Memory Compatibility

This policy aligns with common shell conventions:
- Ctrl+C interrupts (bash, zsh, fish)
- Ctrl+D EOF / logout (bash, zsh)
- Esc clears line in some shells (readline)

### 8.3 Response Time

- All actions must complete within one frame (< 16ms latency).
- No blocking operations in keybinding resolution.
- Visual feedback (input clear, spinner stop) within same frame.

---

## 9) Configuration

### 9.1 Environment Variables

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `FTUI_CTRL_C_IDLE_ACTION` | string | `"quit"` | Action when Ctrl+C in idle state |
| `FTUI_ESC_SEQ_TIMEOUT_MS` | u32 | `250` | Double-Esc detection window |
| `FTUI_ESC_DEBOUNCE_MS` | u32 | `50` | Minimum Esc wait before emit |
| `FTUI_DISABLE_ESC_SEQ` | bool | `false` | Treat all Esc as single |

### 9.2 Runtime Configuration

```rust
pub struct KeybindingConfig {
    /// Action when Ctrl+C pressed with empty input and no task.
    pub ctrl_c_idle_action: CtrlCIdleAction,

    /// Timeout for detecting Esc Esc sequence (ms).
    pub esc_seq_timeout_ms: u32,

    /// Minimum debounce before emitting single Esc (ms).
    pub esc_debounce_ms: u32,

    /// Disable multi-key sequences (strict terminals).
    pub disable_esc_sequences: bool,
}

pub enum CtrlCIdleAction {
    Quit,
    Noop,
    Bell,
}

impl Default for KeybindingConfig {
    fn default() -> Self {
        Self {
            ctrl_c_idle_action: CtrlCIdleAction::Quit,
            esc_seq_timeout_ms: 250,
            esc_debounce_ms: 50,
            disable_esc_sequences: false,
        }
    }
}
```

### 9.3 Keymap File (serde feature)

When the `ftui-core` crate is built with the `serde` feature, a `KeyMap`
round-trips through a human-editable TOML/JSON file. Chords are text
(whitespace-separated combos), contexts are named, and the chord timeout is
milliseconds (clamped to `200..=5000` on load). Unknown keys are rejected so a
typo names itself rather than silently dropping a binding.

```toml
chord_timeout_ms = 750

[[bindings]]
chord = "Ctrl+x Ctrl+s"
action = "Save"
priority = "Mode"
label = "save"

[[bindings]]
chord = "Enter"
action = "Newline"
priority = "Widget"
context = "editor"

[[bindings]]
chord = "g g"
action = "Top"
label = "go to top"
```

This exact example lives at `crates/ftui-core/tests/fixtures/keymap_example.toml`
and is parsed by the test `keybinding::keymap_tests::toml_example_in_docs_parses`,
so the documentation and the parser cannot drift apart.

---

## 10) Failure Modes

| Failure | Cause | Detection | Mitigation |
|---------|-------|-----------|------------|
| Esc Esc false positive | Timeout too long | User feedback, logs | Reduce timeout |
| Esc Esc missed | Timeout too short | User feedback | Increase timeout |
| Ctrl+C ignored | Wrong priority order | Unit test failure | Fix priority table |
| Modal not dismissed | Flag not set | E2E test | Ensure modal sets flag |
| Task not cancelled | Cancel signal lost | Task status check | Verify signal delivery |
| Input not cleared | Buffer not emptied | Visual inspection | Verify buffer.clear() |

### 10.1 Evidence Ledger for Timeout Tuning

For future adaptive tuning, record:
- Inter-arrival time between consecutive Esc presses.
- Whether user intended single or double Esc (inferred from subsequent action).
- Terminal type and multiplexer environment.

Decision rule (Bayes sketch):
```
P(double | dt) = P(dt | double) * P(double) / P(dt)

If dt < ESC_SEQ_TIMEOUT_MS:
  - Assume double-Esc unless subsequent action contradicts.
Else:
  - Emit single Esc immediately.
```

---

## 11) Test Plan

### 11.1 Unit Tests

| Test | Description |
|------|-------------|
| `test_ctrl_c_clears_nonempty_input` | Ctrl+C with text clears buffer |
| `test_ctrl_c_cancels_running_task` | Ctrl+C with task sends cancel |
| `test_ctrl_c_quits_when_idle` | Ctrl+C with no input/task quits |
| `test_esc_dismisses_modal` | Esc closes open modal |
| `test_esc_clears_input_no_modal` | Esc clears input when no modal |
| `test_esc_cancels_task_empty_input` | Esc cancels task when input empty |
| `test_esc_esc_within_timeout` | Two Esc within 250ms toggles tree |
| `test_esc_esc_timeout_expired` | Two Esc with gap > 250ms = two single Esc |
| `test_esc_then_other_key` | Esc + letter emits Esc then letter |
| `test_priority_modal_over_input` | Modal dismiss beats input clear |
| `test_ctrl_c_idle_config_noop` | Config noop prevents quit |
| `test_ctrl_c_idle_config_bell` | Config bell emits BEL |

### 11.2 Property Tests

| Property | Invariant |
|----------|-----------|
| `prop_no_stuck_state` | Sequence detector always returns to Idle |
| `prop_timeout_monotonic` | Longer gap = more likely single Esc |
| `prop_action_deterministic` | Same state + key = same action |
| `prop_modal_priority` | Modal open always takes priority |

### 11.3 E2E Test Scenarios

Each scenario logs JSONL with: `ts`, `key`, `state_flags`, `action`, `latency_us`.

| Scenario | Description |
|----------|-------------|
| `e2e_ctrl_c_sequence` | Type text, Ctrl+C (clear), Ctrl+C (quit) |
| `e2e_esc_modal_dismiss` | Open modal, Esc dismisses, Esc clears input |
| `e2e_esc_esc_tree_toggle` | Rapid Esc Esc opens tree, again closes |
| `e2e_task_cancel_ctrl_c` | Start task, Ctrl+C cancels, status updates |
| `e2e_multiplexer_esc` | Run in tmux with escape-time 0, verify Esc works |
| `e2e_timeout_boundary` | Esc at exactly 249ms, 251ms gap boundaries |

### 11.4 JSONL Log Schema

```json
{
  "ts_ms": 1706900000123,
  "event_id": "evt-001",
  "key": {"code": "Escape", "modifiers": []},
  "state": {
    "input_nonempty": true,
    "task_running": false,
    "modal_open": false,
    "view_overlay": false
  },
  "seq_state": "AwaitingSecondEsc",
  "action": "ClearInput",
  "latency_us": 42,
  "config": {
    "esc_seq_timeout_ms": 250,
    "ctrl_c_idle_action": "quit"
  }
}
```

---

## 12) Implementation Targets

| Module | Responsibility |
|--------|----------------|
| `ftui-core/src/keybinding.rs` | Sequence detector state machine |
| `ftui-runtime/src/action_mapper.rs` | Priority table, state flag queries |
| `ftui-runtime/src/program.rs` | Integrate action mapper in event loop |
| `ftui-harness/src/main.rs` | Reference implementation |

---

## 13) References

- ADR-001: Inline Mode
- ADR-005: One-Writer Rule
- `docs/reference/terminal-compatibility.md`
- `crates/ftui-core/src/event.rs` (KeyCode, Modifiers)

---

## 14) Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-02-02 | GreenCastle | Initial specification (bd-2vne.1) |
| 2026-02-03 | WhiteMarsh | Verified spec completeness against acceptance criteria |
