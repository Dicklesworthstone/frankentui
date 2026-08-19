# FrankenTUI Accessibility

This document describes the current accessibility status of FrankenTUI,
keyboard navigation shortcuts, known limitations, and the roadmap for
full assistive technology (AT) support.

Tracking issue: [#44](https://github.com/Dicklesworthstone/frankentui/issues/44)

---

## Current Status (Phase 2 complete)

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
  accessibility metadata. Widgets that do not implement it are invisible
  to screen readers (treated as presentational/decorative).

### Widgets implementing `Accessible`

| Widget | Role | Key properties exposed |
|---|---|---|
| `TextInput` | TextInput | value/placeholder, focus, mask state |
| `List` | List + ListItem children | block title, item text, item count |
| `Table` | Table | block title, row/column counts |
| `Tabs` | Group + Tab children | tab titles, selected index |
| `ProgressBar` | ProgressBar | ratio, label, value text |
| `Paragraph` | Label | text content (truncated at 200 chars), block title |
| `Block` | Group | title text |
| `Scrollbar` | ScrollBar | orientation (vertical/horizontal) |
| `Spinner` | ProgressBar | label, busy state |

### Focus management system

FrankenTUI has a complete focus management subsystem
(`ftui-widgets::focus`):

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
   the terminal TUI.

2. **Web semantic proxy is static.** The `#a11y-proxy` div in the HTML
   showcase contains a static description. It needs to be dynamically
   updated by the WASM runtime on each render pass to reflect the
   current `A11yTree` snapshot.

3. **Not all widgets implement `Accessible`.** Complex widgets like
   `Modal`, `Toast`, `CommandPalette`, `FilePicker`, `Tree`, and
   `TextArea` do not yet expose accessibility metadata.

4. **Focus indicators are visual only.** Focus changes are not announced
   to screen readers because the platform bridge is not yet connected.

5. **Live regions not wired.** The `LiveRegion` type exists but toasts
   and notifications do not yet emit live-region announcements.

6. **Color contrast not enforced.** The accessibility panel provides a
   high-contrast toggle, but widget styles do not automatically enforce
   WCAG AA/AAA contrast ratios.

---

## Roadmap

### Phase 3: Platform accessibility bridge

- Integrate [AccessKit](https://github.com/AccessKit/accesskit) to
  translate `A11yTreeDiff` into platform accessibility events
  (Windows UIA, macOS Accessibility, Linux AT-SPI).
- The runtime render loop would call `A11yTree::diff()` each frame and
  push changes through the AccessKit adapter.
- Estimated integration point: `ftui-runtime` render thread, after
  `Frame` is finalized but before presentation.

### Phase 4: Web semantic proxy (dynamic)

- After each WASM render pass, serialize the `A11yTree` snapshot into
  semantic HTML and inject it into the `#a11y-proxy` div.
- Each `A11yNodeInfo` maps to an HTML element: `<button>`,
  `<input>`, `<table>`, `<ul>/<li>`, `<div role="...">`, etc.
- Focus changes update `aria-activedescendant` on the proxy container.
- Live-region nodes emit `aria-live` announcements.

### Phase 5: Remaining widget coverage

- Implement `Accessible` for: Modal (Dialog role), Toast (Alert role
  with live region), CommandPalette (Combobox role), TextArea
  (TextInput role, multiline), FilePicker (Tree role), Tree (Tree +
  TreeItem roles).

### Phase 6: Automated accessibility testing

- Add tests that build an `A11yTree` from rendered widget output and
  assert: every interactive widget has a non-empty accessible name,
  focus changes produce correct `A11yTreeDiff` entries, WCAG contrast
  ratios are met in high-contrast mode.
- Collaborate with @AutoSponge on WCAG/ATAG/WCAG2ICT validation.

---

## Contributing

Accessibility improvements are welcome. Key areas where help is needed:

- **AccessKit integration** -- Rust experience with AccessKit's tree
  update API.
- **Web proxy layer** -- JavaScript/WASM experience for dynamic DOM
  manipulation.
- **Widget `Accessible` implementations** -- adding the trait to
  remaining widgets is straightforward (see `input.rs` or `list.rs`
  for examples).
- **Testing** -- screen reader testing on Windows (NVDA/JAWS), macOS
  (VoiceOver), and Linux (Orca).

See the `ftui-a11y` crate's doc comments and tests for examples of
building accessibility trees and verifying diff output.
