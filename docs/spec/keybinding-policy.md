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

pub struct KeyMapConfig {
    /// Maximum gap between keys in a multi-key chord (default: 1000 ms, clamped to 200..=5000 ms).
    pub chord_timeout_ms: u32,
}

impl Default for KeyMapConfig {
    fn default() -> Self {
        Self {
            chord_timeout_ms: 1000,
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
| `chord_completes_within_timeout` | Multi-key chord (`g g`, `Ctrl+x Ctrl+s`) dispatches when completed within `chord_timeout_ms` |
| `widget_beats_mode_beats_global` | `Priority::Widget` overrides `Mode` and `Global` for the same chord |
| `active_context_beats_contextless_even_at_lower_priority` | Context-scoped binding wins when its `ContextId` is active |
| `conflicts_reports_shadowed_prefix_and_duplicate` | `KeyMap::conflicts()` reports duplicate, shadowed, and prefix-overlap bindings |
| `keymap_round_trips_through_toml_and_json` | `KeyMap` round-trips losslessly through TOML and JSON under `serde` |
| `spec_defaults_match_code` | Verifies this spec's `chord_timeout_ms = 1000` and `250` ms Esc timeout match `ftui_core::keybinding` |

### 11.2 Property Tests

| Property | Invariant |
|----------|-----------|
| `prop_no_stuck_state` | Sequence detector always returns to Idle |
| `prop_timeout_monotonic` | Longer gap = more likely single Esc |
| `prop_action_deterministic` | Same state + key = same action |
| `prop_modal_priority` | Modal open always takes priority |

### 11.3 E2E Test Scenarios

Each scenario logs JSONL with: `ts`, `key`, `state_flags`, `action`, `latency_us` (or `key_dispatch` events via `FTUI_DEMO_KEY_JSONL` in `tests/e2e/scripts/test_keybinding_chords.sh`).

| Scenario | Description |
|----------|-------------|
| `e2e_ctrl_c_sequence` | Type text, Ctrl+C (clear), Ctrl+C (quit) |
| `e2e_esc_modal_dismiss` | Open modal, Esc dismisses, Esc clears input |
| `e2e_esc_esc_tree_toggle` | Rapid Esc Esc opens tree, again closes |
| `e2e_task_cancel_ctrl_c` | Start task, Ctrl+C cancels, status updates |
| `e2e_multiplexer_esc` | Run in tmux with escape-time 0, verify Esc works |
| `e2e_timeout_boundary` | Esc at exactly 249ms, 251ms gap boundaries |
| `chord_gg_scrolls_to_top` | `g g` within timeout scrolls `LogSearch` to line 0 (`test_keybinding_chords.sh`) |
| `chord_gg_times_out` | `g` separated by 1300 ms expires twice before a rapid `g g` dispatches `scroll_to_top` |
| `single_key_not_blocked_by_pending_chord` | `g` followed by `j` within 100 ms flushes `g` and dispatches `j` (`scroll_down`) |
| `chord_ctrl_x_ctrl_s_saves` | `Ctrl+x Ctrl+s` in `AdvancedTextEditor` sets `Saved` in the status bar |
| `keybind_regressions` | Runs all 8 `test_keybinding.sh` PTY scenarios (`Ctrl+C`, `Esc`, `Esc Esc`, `Ctrl+D`, `Ctrl+Q`) |

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

When `FTUI_DEMO_KEY_JSONL=<path>` is set on `ftui-demo-showcase`, each `KeyDispatcher` transition also appends a compact JSONL line:

```json
{"event":"key_dispatch","outcome":"dispatched","chord":"g g","action":"scroll_to_top","binding":"scroll_to_top"}
```

where `outcome` is one of `"dispatched"`, `"pending"`, `"expired"`, or `"unbound"`.

---

## 12) Keymaps and Chords

`ftui_core::keybinding` provides a declarative keymap and stateful chord dispatcher above the `SequenceDetector`:

- **Types**: `KeyCombo` (normalized `KeyCode` + `Modifiers`), `Chord` (`SmallVec<[KeyCombo; 4]>` parsed via `Chord::parse("Ctrl+x Ctrl+s")`), `Priority` (`Global < Mode < Widget`), `ContextId` (named activation scope), `Binding<A>`, `KeyMap<A>`, and `KeyDispatcher<'a, A>`.
- **Resolution order**: when multiple bindings match a completed chord, `KeyMap` selects the winner by:
  1. **Active context**: a binding whose `ContextId` is in the active context set beats a context-free binding (bindings for inactive contexts are ignored).
  2. **Priority**: `Priority::Widget` beats `Priority::Mode`, which beats `Priority::Global`.
  3. **Recency**: among equal context specificity and priority, the most recently registered binding wins.
- **Timeout semantics (`chord_timeout_ms = 1000`)**: `DEFAULT_CHORD_TIMEOUT` is 1,000 ms (clamped to `200..=5000` ms). When a key matches both a 1-key binding and a longer chord's prefix, `KeyDispatcher::feed` returns `Dispatch::Pending` and waits up to `chord_timeout`. If `KeyDispatcher::tick(now)` runs after the deadline, it returns `Dispatch::Expired(Some(&action))` for the 1-key fallback (or `Dispatch::Expired(None)` if only the longer chord was bound).
- **Flush rule**: if a pending prefix is followed before timeout by a key that does not extend any active chord, the pending prefix resolves first (emitting its 1-key fallback if bound) and the new key is immediately dispatched from the clean state. Single-key shortcuts are never swallowed by unrelated chord prefixes. `Esc` and `Esc Esc` share the 250 ms `SequenceDetector` window (`esc_seq_timeout_ms: 250`).

---

## 13) Conflict Report

`KeyMap::conflicts()` inspects all registered bindings and returns a `Vec<Conflict<A>>` with three `ConflictKind` variants:

1. **`Duplicate`**: two bindings have the exact same `(chord, priority, context)`. The earlier binding can never fire because the later one wins by recency.
2. **`Shadowed`**: two bindings share `(chord, context)` at different priorities (e.g., `Priority::Widget` vs `Priority::Global`). The higher-priority binding shadows the lower-priority one whenever that context is active.
3. **`PrefixOverlap`**: one binding's chord (`g`) is a strict prefix of another binding's chord (`g g`) in overlapping contexts. Both remain reachable (the 1-key binding fires on timeout or non-matching next key), and the conflict report surfaces the latency trade-off.

Each `Conflict` implements `Display` as a single human-readable diagnostic line.

---

## 14) Serialization

Under the `serde` feature (`ftui-core/serde`, re-exported by `ftui/serde`), `KeyMap<A>` implements `Serialize` and `Deserialize` via a human-editable representation (`chord_timeout_ms` plus `[[bindings]]` entries) and exposes convenience helpers `KeyMap::to_toml`, `KeyMap::from_toml`, `KeyMap::to_json`, and `KeyMap::from_json`. The canonical TOML fixture in Section 9.3 (`crates/ftui-core/tests/fixtures/keymap_example.toml`) is verified on every test run by `keybinding::keymap_tests::toml_example_in_docs_parses`.

---

## 15) Implementation Targets

| Module | Responsibility |
|--------|----------------|
| `ftui-core/src/keybinding.rs` | Sequence detector, `KeyMap`, `Chord`, `KeyDispatcher`, conflict analysis, TOML/JSON serde |
| `ftui-runtime/src/pane_keymap.rs` | Pane keyboard bindings and hint generation via `KeyMap<PaneCommand>` |
| `ftui-widgets/src/help.rs` | `Help::from_keymap` and `KeybindingHints::from_keymap` |
| `ftui-demo-showcase/src/app.rs` | Showcase global and screen-level keymap integration |
| `ftui-harness/src/main.rs` | Reference implementation for `ActionMapper` |

---

## 16) References

- ADR-001: Inline Mode
- ADR-005: One-Writer Rule
- `docs/reference/terminal-compatibility.md`
- `docs/spec/pane-keyboard-interaction.md`
- `crates/ftui-core/src/event.rs` (KeyCode, Modifiers)

---

## 17) Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-02-02 | GreenCastle | Initial specification (bd-2vne.1) |
| 2026-02-03 | WhiteMarsh | Verified spec completeness against acceptance criteria |
| 2026-10-15 | Antigravity | Added Sections 12–14 (`KeyMap`, `Chord`, `KeyDispatcher`, conflict report, serde) and E2E chord test matrix (G14) |
