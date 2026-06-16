# Pane Keyboard Interaction Contract

Status: Normative (bd-21pbi.1)
Owners: `ftui-layout`, `ftui-runtime` (terminal binding bd-21pbi.2), `ftui-web` (web binding bd-21pbi.3)
Last Updated: 2026-06-15

## 1. Why This Exists

Pointer drag-resize is a first-class, host-agnostic pipeline:
`PaneSemanticInputEvent` → `PaneDragResizeMachine` → `PaneTree::operations_for_transition`.
Keyboard (and assistive-technology) interaction MUST be equally first-class and
equally host-agnostic. This document is the normative contract for that layer.
Its executable form is `crates/ftui-layout/src/pane_command.rs`; this spec and
that module MUST agree.

Key non-goals (owned by downstream beads):

- Mapping raw terminal key events to commands, focus-ring rendering, and command
  palette hints — `bd-21pbi.2`.
- Mapping browser `KeyboardEvent`s to commands, roving `tabindex`, and ARIA
  splitter/pane semantics — `bd-21pbi.3`.

This spec defines the **commands and their semantics**, not how any host produces
them. Hosts MUST translate their native key events into `PaneCommand`s and feed
them through `resolve`.

## 2. Command Vocabulary

The canonical command set (`PaneCommand`) is closed and host-agnostic:

| Command | Intent |
|---------|--------|
| `FocusNext` / `FocusPrevious` | Move focus by the cyclic focus order. |
| `FocusDirectional(dir)` | Move focus to the nearest pane in a cardinal direction. |
| `FocusEdge(dir)` | Move focus to the extreme pane in a cardinal direction. |
| `ResizeStep { direction, units }` | Grow/shrink the active pane by `units` snap steps. |
| `Split(axis)` | Split the active leaf along `axis`. |
| `Close` | Close the active leaf, promoting its sibling. |
| `MovePane(dir)` | Dock the active pane against the nearest pane in a direction. |
| `SwapPane(ordinal)` | Swap the active pane with its cyclic neighbour. |
| `Maximize` / `Restore` | Enter/leave a transient maximized view state. |

Commands MUST map to semantic operations, never to host key events. `direction`
in `ResizeStep` is **active-pane-relative**: `Increase` always grows the active
pane regardless of whether it is the first or second child of its enclosing
split (the resolver translates to the split's first-share direction).

## 3. Focus Graph

### 3.1 Focus order (normative tie-break root)

The canonical focus order is the **topological depth-first leaf order**: at every
split, the `first` child subtree is fully visited before the `second`. It is
independent of solved geometry, so it is stable for any layout area and is the
deterministic tie-break of last resort for every other rule below.

`FocusNext`/`FocusPrevious` index into this order and wrap cyclically.

### 3.2 Directional focus (spatial)

`FocusDirectional(dir)` selects among leaves that lie **wholly** on the `dir`
side of the active pane by edge (pane layouts are guillotine splits, i.e.
axis-aligned tilings, so edge containment is unambiguous):

- `Left`: `candidate.right() <= active.left()`
- `Right`: `candidate.left() >= active.right()`
- `Up`: `candidate.bottom() <= active.top()`
- `Down`: `candidate.top() >= active.bottom()`

Among qualifying candidates the winner is chosen by, in order (all
deterministic):

1. smallest primary-axis center distance,
2. then largest perpendicular-axis overlap,
3. then earliest focus-order index.

If no candidate qualifies, the command is a no-op (`NoTargetInDirection`).

### 3.3 Edge focus

`FocusEdge(dir)` selects the single most extreme leaf in `dir` by center
coordinate, tie-broken by the stable perpendicular coordinate and then
focus-order index. When the extreme is already the active pane the command is a
no-op.

## 4. Command → Effect Resolution

`resolve(tree, layout, focus_context, command) -> PaneCommandResolution` is a
**pure function**. Its `effect` is exactly one of:

- `Focus { previous, active }` — transient; no topology change.
- `Structural(Vec<PaneOperation>)` — applied by the host via
  `PaneTree::apply_operation`.
- `Maximize { target }` / `Restore { previous }` — transient view state.
- `Noop(reason)` — see §7.

Mapping:

| Command | Effect | Notes |
|---------|--------|-------|
| Focus* | `Focus` | Active MUST be a leaf; else no-op. |
| `ResizeStep` | `Structural([SetSplitRatio])` | Lowered through `PaneDragResizeMachine` + `operations_for_transition` so keyboard resize uses the **same** nudge math as pointer/wheel (one step = `PANE_SNAP_DEFAULT_STEP_BPS`). |
| `Split(axis)` | `Structural([SplitLeaf])` | New leaf id is allocated on apply; focus stays on the existing pane (host MAY refocus the new leaf). |
| `Close` | `Structural([CloseNode])` | `next_active` is the deterministic survivor (next in focus order, else previous). Root MUST NOT close. |
| `MovePane(dir)` | `Structural([MoveSubtree])` | Dock target is `FocusDirectional(dir)`; axis/placement derive from `dir`. |
| `SwapPane(ord)` | `Structural([SwapNodes])` | Swap with the cyclic neighbour. |
| `Maximize`/`Restore` | `Maximize`/`Restore` | `maximized` is host-owned transient state; no topology change. |

The resolver returns `next_active` and `next_maximized`; the host MUST adopt them
verbatim (this is how focus follows structural changes deterministically).

## 5. Repeat / Acceleration Policy

`ResizeStep.units` is supplied by the host, computed from
`PaneCommandAcceleration`: the first `accelerate_after_repeats` presses use
`base_units`; sustained key repeat escalates to `accelerated_units`. Defaults:
`base_units = 1`, `accelerated_units = 5`, `accelerate_after_repeats = 3`. The
policy is explicit and unit-tested; hosts MUST NOT invent ad-hoc acceleration.

## 6. Keymap Precedence

When both the application and the pane manager bind a key,
`PaneKeymapPrecedence` resolves the owner:

- `PaneManagerFirst` (default): the pane manager wins while it holds focus.
- `ApplicationFirst`: the application wins (for globally reserved shortcuts).

Unbound-by-both yields `Unbound`; bound-by-one yields that layer. Hosts MUST
route a key through `resolve` only when its owner is the pane manager.

## 7. No-op Reasons

`Noop` MUST carry a machine-readable reason for diagnostics:
`NoActivePane`, `ActiveNotLeaf`, `OnlyOnePane`, `NoTargetInDirection`,
`RootCannotClose`, `NoEnclosingSplit`, `AlreadyMaximized`, `NotMaximized`.
Diagnostics for any failed keyboard workflow MUST include the active pane, the
command, and the resolved no-op reason (host context supplied by the binding
layer).

## 8. Determinism & Cross-Host Equivalence (Acceptance)

Because `resolve` is pure over `(tree, layout, focus_context, command)`:

- **Determinism**: the same command stream applied to the same starting tree
  MUST produce a byte-identical final topology hash (`PaneTree::state_hash`) and
  active pane on every run.
- **Cross-host equivalence**: terminal and web hosts that translate their native
  keys into the **same** `PaneCommand` stream MUST reach identical pane state.
  This is the parity guarantee `bd-a46q1` (validation) checks and the property
  `pane_command::tests::command_stream_is_deterministic_and_host_agnostic`
  proves.

Host bindings (`bd-21pbi.2`, `bd-21pbi.3`) are conformant iff, for every
supported key sequence, the `PaneCommand` stream they emit is the documented one
for that sequence.

## 9. Default Host Keymaps

Bindings differ per host because terminals and browsers reserve different keys,
but both MUST emit the same `PaneCommand` for equivalent intent. The defaults:

| Command | Terminal (`ftui_runtime::pane_keymap`) | Web (`ftui_web::pane_keyboard`) |
|---------|----------------------------------------|---------------------------------|
| `FocusNext` / `FocusPrevious` | `Tab` / `Shift+Tab` | `n` / `p` |
| `FocusDirectional` | `Ctrl+Arrows` | `Arrows` |
| `FocusEdge` | `Ctrl+Shift+Arrows` | `Home` / `End` / `PageUp` / `PageDown` |
| `MovePane` | `Alt+Arrows` | `Shift+Arrows` |
| `ResizeStep` grow/shrink | `Alt++` / `Alt+-` | `=` / `-` |
| `Split` H / V | `Alt+s` / `Alt+v` | `s` / `v` |
| `Close` | `Alt+w` | `x` / `Delete` |
| `SwapPane` prev / next | `Alt+[` / `Alt+]` | `[` / `]` |
| `Maximize` / `Restore` | `Alt+z` / `Alt+r` | `f` / `Escape` |

The web keymap MUST refuse any key carrying Ctrl or Super so it never steals
browser/OS shortcuts (e.g. `Ctrl+W`, `Ctrl++`). Both keymaps MUST be gated by
the host so they fire only while the pane manager (not pane content) holds focus
(§6).

## 10. Web Accessibility Semantics (Roving Tabindex + ARIA)

The web host MUST expose a `pane_accessibility_tree` mapping each pane node to
DOM attributes:

- **Roving tabindex** (normative invariant): among pane leaves, exactly the
  effective-active leaf carries `tabindex = 0`; every other leaf carries `-1`.
  When nothing is focused, the first leaf in `focus_order` is the active one, so
  the pane manager is always a single reachable Tab stop with internal arrow-key
  navigation.
- **Panes** (`role="group"`): the active pane additionally sets
  `aria-current="true"`.
- **Splitters** (`role="separator"`): expose `aria-orientation` (a horizontal
  split's divider is `vertical` and vice-versa) and `aria-valuenow`/`valuemin`
  (`0`)/`valuemax` (`100`) carrying the first child's share percentage, so
  assistive tech can identify the active pane, splitter orientation, and resize
  affordances.

## 11. Accessible Announcements

Every effective command (non-no-op) SHOULD emit one concise, deterministic
announcement via `announce_command(command, resolution, tree)`, where `tree` is
the post-apply state:

| Command | Announcement |
|---------|--------------|
| Focus* | `Focused pane <label>` |
| `ResizeStep` | `Resized pane to <pct> percent` (the active pane's own share) |
| `Split(axis)` | `Split pane <axis>, <n> panes` |
| `Close` | `Closed pane, <n> remaining` |
| `MovePane(dir)` | `Moved pane <dir>` |
| `SwapPane` | `Swapped pane` |
| `Maximize` / `Restore` | `Maximized pane` / `Restored pane` |

Announcements MUST be bounded and non-spammy. `PaneAnnouncer` enforces this:
the most recent offer wins (so a burst of resize/key-repeat announcements
coalesces to the final value), and consecutive identical text is suppressed.
Hosts call `take()` once per render / live-region update — the web host into an
`aria-live` region, the terminal host into a status line / log hook. No-ops emit
no announcement. The controllers (`PaneKeyboardController`,
`PaneWebKeyboardController`) expose `take_announcement()` / `pending_announcement()`.

## 12. Adaptive Accessibility Modes

`PaneAccessibilityPreferences { reduced_motion, high_contrast, large_target }`
(host-agnostic, in `ftui_layout::pane_command`) carries three adaptive
preferences applied uniformly across hosts. Both controllers store them
(`with_preferences` / `set_preferences` / `preferences`).

**Invariant (normative): adaptive modes are presentation-only.** None of the
three modes is an input to `resolve` — they do not change the command
vocabulary, the focus graph, or announcements. For a fixed key stream, the
resulting topology, focus, maximize state, and announcement text MUST be
identical regardless of which modes are active. This preserves the determinism
and cross-host equivalence of §8 while layering on adaptive presentation.

| Mode | Effect | Mechanism |
|------|--------|-----------|
| `reduced_motion` | Affordance micro-animations collapse to instant steps; no semantic cue is lost (state still changes), only the in-between motion is removed | `PaneAffordanceMotion` (deterministic, integer-only, clock-free): `reduced()` / `with_reduced_motion(true)` step the hover fade-in and active pulse. Controllers expose `affordance_motion()` |
| `high_contrast` | Splitter / focus-ring affordance colors lift from WCAG AA to AAA against the pane surface | `ftui_style::PaneAffordanceTheme::from_resolved(theme, high_contrast)`; the terminal controller's `focus_ring(theme)` applies it. Colors are derived from theme slots and contrast-clamped — no hardcoded values |
| `large_target` | Splitter handles / hit regions enlarge for precision and touch ergonomics | `PaneAccessibilityPreferences::enlarge_target(base)` — ~150% rounded up with a +1-cell floor, monotonic, never smaller than `base` |

**Host wiring.** The terminal host (`ftui_runtime`) reads the high-contrast
preference through `focus_ring(&ResolvedTheme)` and the reduced-motion
preference through `affordance_motion()`. The web host (`ftui_web`) exposes the
three flags as `data-*` attributes for the workspace root via
`accessibility_dataset()` (`data-pane-reduced-motion`, `data-pane-high-contrast`,
`data-pane-large-target`), so browser CSS applies high-contrast palettes,
reduced-motion overrides, and enlarged hit targets through attribute selectors.
The same host-agnostic preference therefore drives equivalent presentation on
both platforms.
