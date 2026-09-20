# FrankenTUI Accessibility

This document describes the current accessibility status of FrankenTUI,
keyboard navigation shortcuts, known limitations, and the roadmap for
full assistive technology (AT) support.

Tracking issue: [#44](https://github.com/Dicklesworthstone/frankentui/issues/44)

---

## Current Status

### Accessibility tree infrastructure (`ftui-a11y` crate)

FrankenTUI ships a dedicated `ftui-a11y` crate that provides an
ARIA-like accessibility tree for TUI widgets:

- **`A11yRole`** -- 23-variant role enum mapping common TUI widget
  archetypes to ARIA-like semantics (Window, Dialog, Button, TextInput,
  List, Table, ProgressBar, ScrollBar, Tab, etc.).
- **`A11yState`** -- comprehensive state flags: focused, disabled,
  checked, expanded, selected, readonly, required, busy, and numeric
  value range/text.
- **`A11yNodeInfo`** -- per-widget node with builder API for ergonomic
  construction. Carries name, description, shortcut hint, bounding
  rectangle, parent/child IDs, and live-region policy.
- **`A11yTree`** -- immutable snapshot built once per render pass via
  `A11yTreeBuilder`. Supports O(1) node lookup, ancestor traversal, and
  children-of queries.
- **`A11yTree::diff()`** -- O(n) tree diffing producing `A11yTreeDiff`
  with granular change detection (name, role, state, bounds, children,
  live region changes, and focus transitions).
- **`Accessible` trait** -- opt-in trait for widgets to provide
  accessibility metadata. Stateful widgets can also contribute nodes
  directly during rendering, as `Dialog` does using its `DialogState`.
  Widgets that contribute no nodes are absent from the accessibility tree.

### Runtime wiring (per-frame tree, diff, announcements)

`Frame` carries an optional `A11yTreeBuilder`. Widgets contribute metadata
while rendering through `frame.push_a11y_nodes`, `frame.push_a11y`, or
`frame.with_a11y_scope` for a container and its descendants. Enable collection
with `ProgramConfig::with_accessibility(ScreenReaderPolicy)`; the default
config collects nothing.

Per rendered frame the runtime then:

1. builds the `A11yTree` (the first parentless node becomes the root
   unless the view set one; the first node whose state reports `focused`
   becomes the tree focus unless a focused ID was set explicitly) and
   records the reading order (push order);
2. diffs it against the previous frame's tree;
3. derives bounded screen-reader announcements (`ScreenReaderPolicy`
   caps count and text length): focus changes, retained-focus control
   state changes, live-region additions and live-content changes;
4. exports `a11y_tree` and `a11y_announcement` metadata to the evidence sink,
   logs metadata on the `ftui.a11y` tracing target, and calls
   `Model::on_accessibility(AccessibilityFrame)` when the tree changed
   (one extra frame is scheduled so state changed there is rendered).

Retained-focus feedback covers `disabled`, `readonly`, `required`, `checked`,
`expanded`, and `selected`, plus slider/scrollbar values. It does not require
making the control a live region. Standalone changes are polite; same-node
focus/state/live updates coalesce before the batch cap. Ordinary text editing
remains in the raw tree diff without repeatedly reading the whole input.

Announcement text remains available to the local `Model::on_accessibility`
callback and the accessors below. Ordinary tracing, including OpenTelemetry
export, never receives that text. Evidence rows default to `"text": null`
and retain node ID, reason, urgency and character count. To deliberately
capture content in a configured evidence sink, use
`ProgramConfig::with_accessibility_evidence_text(true)`. This opt-in may record
application labels or edited text; use it only for a destination intended to
receive that content. It does not enable text in ordinary tracing.

`Program::accessibility_tree()`, `accessibility_order()`,
`accessibility_announcements()` and `accessibility_dump()` expose the same
data for tests and tooling. The demo showcase enables this and its
`accessibility_panel` screen mirrors the tree size, the leading lines of
the dump and the latest announcements.

Existing runtime coverage includes `headless_accessibility_builds_tree_and_exports_announcements`
(ftui-runtime), `finish_a11y_derives_focus_from_node_state` (ftui-render),
`rendering_accessible_widgets_pushes_their_nodes_into_the_frame`
(ftui-widgets), `a11y_tree_dump_dashboard_80x24` (showcase), and the
`pty_focus_announcements` step of `scripts/a11y_transitions_e2e.sh`, which
runs the showcase under a PTY and requires `FocusChanged` rows.

What is **not** done:

- No operating-system bridge (AT-SPI, UIA, NSAccessibility). Announcements
  reach the local callback/accessors; telemetry carries metadata and an
  explicitly opted-in evidence sink can carry text. Nothing reaches a
  screen reader on its own.
- Generic `Modal<C>` content, Tabs content panes, and pane workspaces do not
  automatically gain a semantic container. Built-in `Dialog` presets do
  contribute their own scoped nodes. `Form` still does not contribute its
  fields, so Tab moves inside a form are invisible to the tree.
- Focus is derived from `A11yState::focused` on the pushed nodes (first in
  reading order) unless the view sets a focused ID on the builder; integrating
  application-wide focus ownership with the accessibility tree is future work.

### Widgets contributing accessibility metadata

| Widget | Role | Key properties exposed |
|---|---|---|
| `TextInput` | TextInput | value/placeholder, focus, mask state |
| `TextArea` | TextInput | full multiline value or placeholder, focus, multiline description |
| `List` | List + ListItem children | block title, item text, item count |
| `Table` | Table | block title, row/column counts |
| `Tabs` | Group + Tab children | tab titles, selected index |
| `ProgressBar` | ProgressBar | ratio, label, value text |
| `Paragraph` | Label | text content (truncated at 200 chars), block title |
| `Block` | Group | title text |
| `Scrollbar` | ScrollBar | orientation (vertical/horizontal) |
| `Spinner` | ProgressBar | label, busy state |
| `Dialog` | Dialog + TextInput/Button children | title, message description, visible controls, input value, state-owned focus |

### Built-in dialogs

`Dialog::alert`, `confirm`, `prompt`, and custom dialogs contribute semantics
from their real `StatefulWidget::render` path when accessibility is enabled.
The dialog node covers the content, not its backdrop. Prompt inputs and buttons
are its children in reading order, with bounds clipped to the same scissor used
for hit testing. Closed, empty, or fully clipped dialogs contribute no nodes;
clipped-out controls cannot become phantom accessibility focus targets.

Use a distinct `.hit_id(HitId::new(...))` for each concurrently rendered dialog.
The same ID keeps the dialog and its logical controls stable across moves,
resizes, button-label changes, and input edits. Without a hit ID, the dialog uses
a bounds-derived fallback, so moving or resizing it changes its identity.
Button child IDs use the application button key, with an occurrence index to
keep duplicate keys distinct. The `DIALOG_HIT_INPUT` and `DIALOG_HIT_BUTTON`
tags are both available from `ftui_widgets::modal` for host mouse routing.

Focus follows `DialogState`: prompt input takes precedence over a stale button
index, and otherwise the selected button index supplies visual and semantic
focus. Tab moves forward; both Shift+Tab representations (`Tab` with SHIFT and
`BackTab`) move backward. Invalid retained button indices recover during keyboard
navigation rather than overflowing index arithmetic.

The dialog is not itself focused and is not a live region. Its decorative border
does not add a duplicate named Group. Focus announcements therefore use the
existing bounded diff channel, and typing changes the input value without
reannouncing the whole field. Full labels, descriptions, and input values remain
in the local tree even when visually truncated.

**Application responsibilities remain explicit.** `DialogState::new()` starts
prompt input focus; an alert/confirm/custom-dialog owner chooses the initial
button using `focused_button` (and clears `input_focused` for non-prompts). The
owner must stop routing input to background controls, clear their focus flags,
and restore focus when the dialog closes. Rendering dialog metadata neither
makes background content inert nor connects an operating-system accessibility
bridge. A native-event bridge should not also replay equivalent synthetic speech.

Added regression coverage:

- `crates/ftui-widgets/tests/modal_dialog_accessibility.rs` exercises actual
  rendering and events: presets, scopes, exact hit bounds, both reverse-Tab
  encodings, clipping, stable IDs, duplicate button keys, input edits, explicit
  focus ownership, and unchanged cells/hits/cursor with collection enabled.
- `dialog_runtime::runtime_dialog_events_reach_accessibility_and_restore_caller_focus`
  and `dialog_runtime::runtime_dialog_focus_feedback_does_not_duplicate_under_one_message_cap`
  in `crates/ftui-harness/tests/a11y_interaction_feedback.rs` exercise the real
  `Program` command/update/render/callback path. They open a prompt from a real
  `TextInput`, edit it, navigate buttons, submit, and restore caller focus.

These are executable regression specifications, not a claim of completed
screen-reader interoperability testing. Run their Cargo targets through the
repository's pinned-toolchain DSR verification path, along with the normal
compiler, formatting, Clippy, and documentation gates.

### Focus management system

FrankenTUI has a focus management subsystem (`ftui-widgets::focus`):

- **`FocusGraph`** -- directed graph encoding focus navigation
  relationships (up/down/left/right/next/prev). O(1) navigation.
- **`FocusManager`** -- manages focus state, tab-order traversal, and
  focus groups.
- **`FocusIndicator`** -- configurable visual focus cues: reverse video
  overlay (default), underline, border highlight, or none.
- **`FocusTrap`** -- constrains tab navigation within modal regions
  (dialogs, command palette).
- **Spatial navigation** -- arrow-key navigation based on widget
  bounding rectangles.
- **Tab-order** -- `tab_index` on focus nodes, with ascending-order
  traversal.

### Web rendering (canvas + semantic proxy)

The web showcase (`crates/ftui-showcase-wasm/frankentui_showcase_demo.html`) renders via HTML
`<canvas>` with WebGPU. The canvas element now carries:

- `role="application"` and `aria-label` for screen-reader identification.
- An `#a11y-proxy` div (visually hidden, screen-reader-visible) that
  provides a semantic description of the TUI state. The WASM runtime
  should update this div on each render pass to mirror the `A11yTree`.

---

## Keyboard Navigation Reference

### Global shortcuts (Demo Showcase)

| Key | Action |
|---|---|
| `Tab` | Next screen |
| `Shift+Tab` | Previous screen |
| `Shift+L` | Next screen (Vim-style) |
| `Shift+H` | Previous screen (Vim-style) |
| `0`-`9` | Jump to screen by number |
| `q` | Quit (suppressed when text input is active) |
| `Ctrl+C` | Quit |
| `?` | Toggle help overlay |
| `F12` | Toggle debug overlay |
| `Ctrl+K` | Open command palette |
| `Ctrl+P` | Toggle performance HUD |
| `Ctrl+I` | Toggle inspector / evidence ledger |
| `Ctrl+T` | Cycle theme |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |
| `Shift+A` | Toggle accessibility panel |
| `m` | Toggle mouse capture |
| `F6` | Toggle mouse capture (alternative) |
| `Enter` | Activate highlighted item (on Dashboard) |
| `R` | Retry errored screen |

### Accessibility panel shortcuts

| Key | Action |
|---|---|
| `h` | Toggle high-contrast mode |
| `m` | Toggle reduced-motion mode |
| `l` | Toggle large-text mode |

### Within-widget navigation

Most interactive widgets respond to standard terminal key conventions:

- **Arrow keys** -- navigate items in lists, tables, tabs
- **Enter / Space** -- activate selected item, toggle checkbox
- **Escape** -- close modal/dialog, deactivate focused element
- **Home / End** -- jump to first/last item
- **Page Up / Page Down** -- scroll by viewport height
- **Tab / Shift+Tab** -- move between focusable widgets

---

## Known Limitations

1. **No platform accessibility bridge yet.** The `A11yTree` and
   `A11yTreeDiff` are generated but not yet consumed by a platform
   bridge (AccessKit, AT-SPI, etc.). Screen readers cannot yet read
   the terminal TUI through FrankenTUI's semantic tree on their own.

2. **Web semantic proxy is static.** The `#a11y-proxy` div in the HTML
   showcase contains a static description. It needs to be dynamically
   updated by the WASM runtime on each render pass to reflect the
   current `A11yTree` snapshot.

3. **Widget coverage and container scoping are incomplete.** Generic
   `Modal<C>` content and `Form` fields still need render-path semantics.
   Built-in `Dialog` presets and `TextArea` already contribute metadata.

4. **Focus feedback is not yet delivered to a native screen reader.**
   Tree focus changes produce bounded announcements in the local callback;
   the platform bridge is still absent.

5. **Notification live regions are not wired.** The tree announcement
   machinery handles explicitly designated live regions, but toasts and
   notifications do not yet contribute their own live-region metadata.

6. **Color contrast not enforced.** The accessibility panel provides a
   high-contrast toggle, but widget styles do not automatically enforce
   WCAG AA/AAA contrast ratios.

---

## Roadmap

### Phase 3: Platform accessibility bridge

- Integrate [AccessKit](https://github.com/AccessKit/accesskit) to
  translate `A11yTreeDiff` into platform accessibility events
  (Windows UIA, macOS Accessibility, Linux AT-SPI).
- The runtime already builds and diffs the tree after rendering; add
  delivery to the platform adapter without replaying duplicate synthetic speech.

### Phase 4: Web semantic proxy (dynamic)

- After each WASM render pass, serialize the `A11yTree` snapshot into
  semantic HTML and inject it into the `#a11y-proxy` div.
- Each `A11yNodeInfo` maps to an HTML element: `<button>`,
  `<input>`, `<table>`, `<ul>/<li>`, `<div role="...">`, etc.
- Focus changes update `aria-activedescendant` on the proxy container.
- Live-region nodes emit `aria-live` announcements.

### Phase 5: Remaining widget coverage

- Add render-path metadata for generic Modal content and Form fields, and
  audit specialized widgets such as Toast, CommandPalette, FilePicker, and Tree
  for names, state, focus and hierarchy coverage. Extend the role vocabulary
  where needed. Built-in Dialog presets and multiline TextArea have metadata.

### Phase 6: Automated accessibility testing

- Extend rendered-widget and runtime coverage to every interactive widget,
  checking names, focus ownership, diffs, duplicate suppression, and color
  contrast in high-contrast mode.
- Collaborate with @AutoSponge on WCAG/ATAG/WCAG2ICT validation.

---

## Contributing

Accessibility improvements are welcome. Key areas where help is needed:

- **AccessKit integration** -- Rust experience with AccessKit's tree
  update API.
- **Web proxy layer** -- JavaScript/WASM experience for dynamic DOM
  manipulation.
- **Widget metadata** -- add `Accessible` or stateful render-path metadata
  to remaining widgets; see `input.rs`, `list.rs`, and `modal/dialog.rs`.
- **Testing** -- screen reader testing on Windows (NVDA/JAWS), macOS
  (VoiceOver), and Linux (Orca).

See the `ftui-a11y` crate's doc comments and tests for examples of
building accessibility trees and verifying diff output.
