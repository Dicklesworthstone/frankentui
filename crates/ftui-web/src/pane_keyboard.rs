//! Web pane keyboard bindings + roving tabindex and ARIA semantics (bd-21pbi.3).
//!
//! This is the browser host's binding layer over the canonical command model in
//! `ftui_layout::pane_command` (bd-21pbi.1), the web counterpart to the terminal
//! binding in `ftui_runtime::pane_keymap` (bd-21pbi.2). It provides:
//!
//! - [`key_to_pane_command`]: a **browser-safe** keymap. It refuses any key
//!   carrying Ctrl or Super (Meta/Cmd) so it never steals browser/OS shortcuts
//!   (Ctrl+W close-tab, Ctrl+T, Ctrl++ zoom, Cmd+…). It binds only plain and
//!   Shift-qualified keys, gated by the host to fire only while the pane
//!   container (not pane content) holds focus.
//! - [`pane_accessibility_tree`]: the roving-tabindex + ARIA model. Exactly one
//!   leaf (the active pane) carries `tabindex = 0`; all other leaves carry
//!   `-1`. Splitters expose `role="separator"`, `aria-orientation`, and
//!   `aria-valuenow/min/max` (the split ratio), so assistive tech can identify
//!   the active pane, splitter orientation, and resize affordances.
//! - [`PaneWebKeyboardController`]: a reusable host helper that translates a
//!   key, resolves it, applies structural operations, tracks focus, and exposes
//!   the up-to-date accessibility tree.
//!
//! Cross-host parity: the bindings differ from the terminal host (browsers
//! reserve different keys), but both emit the **same** [`PaneCommand`] for
//! equivalent intent, so both hosts reach identical pane state — the parity
//! guarantee the validation epic checks.

use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_layout::{
    PaneAccessibilityPreferences, PaneAffordanceMotion, PaneAnnouncement, PaneAnnouncer,
    PaneCardinalDirection, PaneCommand, PaneCommandAcceleration, PaneCommandEffect,
    PaneCommandResolution, PaneFocusContext, PaneFocusOrdinal, PaneId, PaneLayout, PaneNodeKind,
    PaneResizeDirection, PaneTree, SplitAxis, announce_command, focus_order, resolve_pane_command,
};

/// Translate a browser key event into a canonical [`PaneCommand`].
///
/// Returns `None` for key releases, for any key carrying Ctrl or Super (those
/// are reserved by the browser/OS and must never be stolen), and for unbound
/// keys. The default browser-safe keymap (pane container focused):
///
/// | Key | Command |
/// |-----|---------|
/// | Arrows | `FocusDirectional` |
/// | Shift+Arrows | `MovePane` |
/// | Home / End / PageUp / PageDown | `FocusEdge` left / right / up / down |
/// | `n` / `p` | `FocusNext` / `FocusPrevious` |
/// | `=` / `-` | `ResizeStep` grow / shrink |
/// | `s` / `v` | `Split` horizontal / vertical |
/// | `x` / `Delete` | `Close` |
/// | `[` / `]` | `SwapPane` previous / next |
/// | `f` / `Escape` | `Maximize` / `Restore` |
///
/// `resize_units` is the snap-step count for `ResizeStep` (computed by the
/// caller via [`PaneCommandAcceleration`]); it is ignored for other commands.
#[must_use]
pub fn key_to_pane_command(key: &KeyEvent, resize_units: u16) -> Option<PaneCommand> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let m = key.modifiers;
    // Never consume browser/OS-reserved chords. Alt is included: Alt+Arrow is
    // browser Back/Forward on Windows/Linux, and this keymap binds only plain
    // and Shift-qualified keys anyway.
    if m.contains(Modifiers::CTRL) || m.contains(Modifiers::SUPER) || m.contains(Modifiers::ALT) {
        return None;
    }
    let shift = m.contains(Modifiers::SHIFT);

    match key.code {
        KeyCode::Left if shift => Some(PaneCommand::MovePane(PaneCardinalDirection::Left)),
        KeyCode::Right if shift => Some(PaneCommand::MovePane(PaneCardinalDirection::Right)),
        KeyCode::Up if shift => Some(PaneCommand::MovePane(PaneCardinalDirection::Up)),
        KeyCode::Down if shift => Some(PaneCommand::MovePane(PaneCardinalDirection::Down)),

        KeyCode::Left => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Left)),
        KeyCode::Right => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Right)),
        KeyCode::Up => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Up)),
        KeyCode::Down => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Down)),

        KeyCode::Home => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Left)),
        KeyCode::End => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Right)),
        KeyCode::PageUp => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Up)),
        KeyCode::PageDown => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Down)),

        KeyCode::Char('n') => Some(PaneCommand::FocusNext),
        KeyCode::Char('p') => Some(PaneCommand::FocusPrevious),

        KeyCode::Char('=' | '+') => Some(PaneCommand::ResizeStep {
            direction: PaneResizeDirection::Increase,
            units: resize_units,
        }),
        KeyCode::Char('-' | '_') => Some(PaneCommand::ResizeStep {
            direction: PaneResizeDirection::Decrease,
            units: resize_units,
        }),

        KeyCode::Char('s' | 'S') => Some(PaneCommand::Split(SplitAxis::Horizontal)),
        KeyCode::Char('v' | 'V') => Some(PaneCommand::Split(SplitAxis::Vertical)),
        KeyCode::Char('x' | 'X') | KeyCode::Delete => Some(PaneCommand::Close),
        KeyCode::Char('[') => Some(PaneCommand::SwapPane(PaneFocusOrdinal::Previous)),
        KeyCode::Char(']') => Some(PaneCommand::SwapPane(PaneFocusOrdinal::Next)),
        KeyCode::Char('f' | 'F') => Some(PaneCommand::Maximize),
        KeyCode::Escape => Some(PaneCommand::Restore),

        _ => None,
    }
}

/// ARIA role for a pane node in the accessibility tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAriaRole {
    /// A pane (leaf): `role="group"`.
    Group,
    /// A splitter (split node): `role="separator"`.
    Separator,
}

impl PaneAriaRole {
    /// The DOM `role` attribute value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Separator => "separator",
        }
    }
}

/// ARIA orientation of a splitter (the divider line's orientation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAriaOrientation {
    /// A horizontal divider line (between vertically-stacked panes).
    Horizontal,
    /// A vertical divider line (between side-by-side panes).
    Vertical,
}

impl PaneAriaOrientation {
    /// The DOM `aria-orientation` attribute value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

/// One node in the pane accessibility tree, mapping directly to DOM ARIA
/// attributes the web host applies to its pane/splitter elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneAriaNode {
    /// The pane node this describes.
    pub pane_id: PaneId,
    /// `role`.
    pub role: PaneAriaRole,
    /// `tabindex`. Roving: exactly one leaf (the active pane) is `0`; all other
    /// leaves are `-1`. Splitters are `-1` (described, not tab-focused).
    pub tabindex: i32,
    /// `aria-label`.
    pub label: String,
    /// `aria-orientation` (splitters only).
    pub orientation: Option<PaneAriaOrientation>,
    /// `aria-valuenow` — splitter first-child share, 0..=100 (splitters only).
    pub value_now: Option<u16>,
    /// `aria-valuemin` (splitters only).
    pub value_min: Option<u16>,
    /// `aria-valuemax` (splitters only).
    pub value_max: Option<u16>,
    /// `aria-current="true"` / `aria-selected` — the active pane.
    pub current: bool,
}

/// Compute the roving-tabindex + ARIA accessibility tree for a pane workspace.
///
/// The roving invariant: among leaves, exactly the effective-active leaf (the
/// focused pane, or the first pane in focus order when nothing is focused)
/// carries `tabindex = 0`; every other leaf carries `-1`. This makes the whole
/// pane manager a single Tab stop whose internal navigation is driven by the
/// keymap above.
#[must_use]
pub fn pane_accessibility_tree(tree: &PaneTree, focus: PaneFocusContext) -> Vec<PaneAriaNode> {
    let order = focus_order(tree);
    let effective_active = focus
        .active
        .filter(|id| order.contains(id))
        .or_else(|| order.first().copied());

    let mut nodes = Vec::new();
    for record in tree.nodes() {
        match &record.kind {
            PaneNodeKind::Leaf(leaf) => {
                let is_active = Some(record.id) == effective_active;
                nodes.push(PaneAriaNode {
                    pane_id: record.id,
                    role: PaneAriaRole::Group,
                    tabindex: i32::from(is_active) - 1, // true -> 0, false -> -1
                    label: format!("pane {}", leaf.surface_key),
                    orientation: None,
                    value_now: None,
                    value_min: None,
                    value_max: None,
                    current: is_active,
                });
            }
            PaneNodeKind::Split(split) => {
                // A horizontal split (side-by-side children) has a vertical
                // divider, and vice versa.
                let orientation = match split.axis {
                    SplitAxis::Horizontal => PaneAriaOrientation::Vertical,
                    SplitAxis::Vertical => PaneAriaOrientation::Horizontal,
                };
                // u64 arithmetic: `num + den` can overflow u32 (any nonzero
                // pair is a valid ratio), which would wrap into a division by
                // zero in release builds — unwrap_or cannot catch a panic.
                let num = u64::from(split.ratio.numerator());
                let den = u64::from(split.ratio.denominator());
                let first_share = u16::try_from(num * 100 / (num + den)).unwrap_or(50);
                nodes.push(PaneAriaNode {
                    pane_id: record.id,
                    role: PaneAriaRole::Separator,
                    tabindex: -1,
                    label: format!("{} pane divider", orientation.as_str()),
                    orientation: Some(orientation),
                    value_now: Some(first_share),
                    value_min: Some(0),
                    value_max: Some(100),
                    current: false,
                });
            }
        }
    }
    nodes
}

/// Outcome of feeding one key event to a [`PaneWebKeyboardController`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneWebKeyOutcome {
    /// Not a pane binding (or browser-reserved); the host handles it normally.
    Unbound,
    /// The command was resolved (and any structural operations applied).
    Handled {
        /// The command the key mapped to.
        command: PaneCommand,
        /// The full resolution.
        resolution: PaneCommandResolution,
    },
    /// A structural operation failed to apply; focus state is left unchanged.
    Failed {
        /// The command that failed.
        command: PaneCommand,
    },
}

/// A reusable web pane keyboard host: browser-safe keymap + canonical resolution
/// + focus tracking + a live roving-tabindex/ARIA tree.
#[derive(Debug, Clone)]
pub struct PaneWebKeyboardController {
    focus: PaneFocusContext,
    acceleration: PaneCommandAcceleration,
    op_seed: u64,
    repeat_count: u16,
    announcer: PaneAnnouncer,
    preferences: PaneAccessibilityPreferences,
}

impl PaneWebKeyboardController {
    /// Create a controller focused on `active` (or unfocused if `None`).
    #[must_use]
    pub fn new(active: Option<PaneId>) -> Self {
        Self {
            focus: PaneFocusContext {
                active,
                maximized: None,
            },
            acceleration: PaneCommandAcceleration::default(),
            op_seed: 0,
            repeat_count: 0,
            announcer: PaneAnnouncer::new(),
            preferences: PaneAccessibilityPreferences::none(),
        }
    }

    /// Take the pending accessibility announcement (for an `aria-live` region),
    /// coalesced and de-duplicated. Call once per render / live-region update.
    pub fn take_announcement(&mut self) -> Option<PaneAnnouncement> {
        self.announcer.take()
    }

    /// Peek the pending accessibility announcement without consuming it.
    #[must_use]
    pub fn pending_announcement(&self) -> Option<&PaneAnnouncement> {
        self.announcer.pending()
    }

    /// Override the resize acceleration policy.
    #[must_use]
    pub const fn with_acceleration(mut self, acceleration: PaneCommandAcceleration) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// Set the accessibility preferences (reduced-motion / high-contrast /
    /// large-target) at construction time (bd-21pbi.5).
    #[must_use]
    pub const fn with_preferences(mut self, preferences: PaneAccessibilityPreferences) -> Self {
        self.preferences = preferences;
        self
    }

    /// Update the accessibility preferences at runtime (e.g. when the browser
    /// reports a `prefers-reduced-motion` / `prefers-contrast` media-query
    /// change). Never affects focus/command semantics — presentation only.
    pub const fn set_preferences(&mut self, preferences: PaneAccessibilityPreferences) {
        self.preferences = preferences;
    }

    /// The active accessibility preferences.
    #[must_use]
    pub const fn preferences(&self) -> PaneAccessibilityPreferences {
        self.preferences
    }

    /// The affordance micro-animation policy implied by the current
    /// preferences (motion is stepped under reduced-motion).
    #[must_use]
    pub fn affordance_motion(&self) -> PaneAffordanceMotion {
        self.preferences.affordance_motion()
    }

    /// Accessibility state as `data-*` attribute pairs for the workspace root.
    ///
    /// The browser host stamps these on the pane container so CSS can apply
    /// high-contrast palettes, reduced-motion overrides, and enlarged hit
    /// targets via attribute selectors (e.g.
    /// `[data-pane-high-contrast="true"] .pane-splitter { … }`). This is the web
    /// counterpart to the terminal host's themed focus ring + sizing: the same
    /// host-agnostic preference drives presentation on both platforms.
    #[must_use]
    pub fn accessibility_dataset(&self) -> [(&'static str, bool); 3] {
        [
            ("data-pane-reduced-motion", self.preferences.reduced_motion),
            ("data-pane-high-contrast", self.preferences.high_contrast),
            ("data-pane-large-target", self.preferences.large_target),
        ]
    }

    /// The current focus context.
    #[must_use]
    pub const fn focus(&self) -> PaneFocusContext {
        self.focus
    }

    /// The currently active (focused) pane.
    #[must_use]
    pub const fn active(&self) -> Option<PaneId> {
        self.focus.active
    }

    /// The currently maximized pane, if any.
    #[must_use]
    pub const fn maximized(&self) -> Option<PaneId> {
        self.focus.maximized
    }

    /// Set the active pane (e.g. when a pointer interaction changes focus).
    pub const fn set_active(&mut self, active: Option<PaneId>) {
        self.focus.active = active;
    }

    /// The current roving-tabindex + ARIA tree for `tree`, reflecting focus.
    #[must_use]
    pub fn accessibility_tree(&self, tree: &PaneTree) -> Vec<PaneAriaNode> {
        pane_accessibility_tree(tree, self.focus)
    }

    /// Translate, resolve, and apply one browser key event. Structural commands
    /// mutate `tree`; focus/maximize commands update internal state. `layout`
    /// must be the solved layout of `tree`.
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        tree: &mut PaneTree,
        layout: &PaneLayout,
    ) -> PaneWebKeyOutcome {
        if key.kind == KeyEventKind::Repeat {
            self.repeat_count = self.repeat_count.saturating_add(1);
        } else {
            self.repeat_count = 0;
        }
        let resize_units = self.acceleration.units_for(self.repeat_count);

        let Some(command) = key_to_pane_command(key, resize_units) else {
            return PaneWebKeyOutcome::Unbound;
        };

        let resolution = resolve_pane_command(tree, layout, self.focus, command);
        if let PaneCommandEffect::Structural(ops) = &resolution.effect {
            for op in ops {
                if tree.apply_operation(self.op_seed, op.clone()).is_err() {
                    return PaneWebKeyOutcome::Failed { command };
                }
                self.op_seed += 1;
            }
        }
        self.focus.active = resolution.next_active;
        self.focus.maximized = resolution.next_maximized;
        self.announcer
            .offer(announce_command(command, &resolution, tree));
        PaneWebKeyOutcome::Handled {
            command,
            resolution,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_layout::{
        PANE_TREE_SCHEMA_VERSION, PaneLeaf, PaneNodeRecord, PaneSplit, PaneSplitRatio,
        PaneTreeSnapshot, Rect,
    };
    use std::collections::BTreeMap;

    fn pid(raw: u64) -> PaneId {
        PaneId::new(raw).expect("non-zero id")
    }

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }

    #[test]
    fn alt_chords_are_never_bound() {
        // Alt+Arrow is browser Back/Forward on Windows/Linux; the browser-safe
        // keymap must ignore every Alt-qualified chord.
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Left, Modifiers::ALT), 1),
            None
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Right, Modifiers::ALT), 1),
            None
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Left, Modifiers::SHIFT | Modifiers::ALT), 1),
            None
        );
    }

    /// Horizontal root: left(2) | vertical(top(4)/bottom(5)). Focus order [2,4,5].
    fn nested() -> PaneTree {
        let snapshot = PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root: pid(1),
            next_id: pid(6),
            nodes: vec![
                PaneNodeRecord::split(
                    pid(1),
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(1, 1).unwrap(),
                        first: pid(2),
                        second: pid(3),
                    },
                ),
                PaneNodeRecord::leaf(pid(2), Some(pid(1)), PaneLeaf::new("left")),
                PaneNodeRecord::split(
                    pid(3),
                    Some(pid(1)),
                    PaneSplit {
                        axis: SplitAxis::Vertical,
                        ratio: PaneSplitRatio::new(3, 1).unwrap(),
                        first: pid(4),
                        second: pid(5),
                    },
                ),
                PaneNodeRecord::leaf(pid(4), Some(pid(3)), PaneLeaf::new("right_top")),
                PaneNodeRecord::leaf(pid(5), Some(pid(3)), PaneLeaf::new("right_bottom")),
            ],
            extensions: BTreeMap::new(),
        };
        PaneTree::from_snapshot(snapshot).expect("valid tree")
    }

    #[test]
    fn web_keymap_is_browser_safe() {
        // Any Ctrl/Super chord is never consumed (browser/OS reserved).
        for code in [
            KeyCode::Char('w'),
            KeyCode::Char('t'),
            KeyCode::Char('='),
            KeyCode::Left,
        ] {
            assert_eq!(key_to_pane_command(&key(code, Modifiers::CTRL), 1), None);
            assert_eq!(key_to_pane_command(&key(code, Modifiers::SUPER), 1), None);
        }
        // Release events ignored.
        let mut release = key(KeyCode::Left, Modifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_to_pane_command(&release, 1), None);
    }

    #[test]
    fn web_keymap_covers_the_vocabulary() {
        let none = Modifiers::NONE;
        let shift = Modifiers::SHIFT;
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Left, none), 1),
            Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Left))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Down, shift), 1),
            Some(PaneCommand::MovePane(PaneCardinalDirection::Down))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Home, none), 1),
            Some(PaneCommand::FocusEdge(PaneCardinalDirection::Left))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('n'), none), 1),
            Some(PaneCommand::FocusNext)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('='), none), 3),
            Some(PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Increase,
                units: 3
            })
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('s'), none), 1),
            Some(PaneCommand::Split(SplitAxis::Horizontal))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Delete, none), 1),
            Some(PaneCommand::Close)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char(']'), none), 1),
            Some(PaneCommand::SwapPane(PaneFocusOrdinal::Next))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('f'), none), 1),
            Some(PaneCommand::Maximize)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Escape, none), 1),
            Some(PaneCommand::Restore)
        );
    }

    #[test]
    fn roving_tabindex_has_exactly_one_zero() {
        let tree = nested();
        let nodes = pane_accessibility_tree(&tree, PaneFocusContext::active(pid(4)));
        let zeros: Vec<_> = nodes
            .iter()
            .filter(|n| n.role == PaneAriaRole::Group && n.tabindex == 0)
            .collect();
        assert_eq!(zeros.len(), 1, "exactly one leaf is the roving Tab stop");
        assert_eq!(zeros[0].pane_id, pid(4));
        assert!(zeros[0].current);
        // Every other leaf is -1.
        for n in nodes
            .iter()
            .filter(|n| n.role == PaneAriaRole::Group && n.pane_id != pid(4))
        {
            assert_eq!(n.tabindex, -1);
            assert!(!n.current);
        }
    }

    #[test]
    fn roving_tabindex_defaults_to_first_pane_when_unfocused() {
        let tree = nested();
        let nodes = pane_accessibility_tree(&tree, PaneFocusContext::default());
        let active: Vec<_> = nodes.iter().filter(|n| n.tabindex == 0).collect();
        assert_eq!(active.len(), 1);
        // First leaf in focus order is left(2).
        assert_eq!(active[0].pane_id, pid(2));
    }

    #[test]
    fn separator_exposes_orientation_and_value() {
        let tree = nested();
        let nodes = pane_accessibility_tree(&tree, PaneFocusContext::active(pid(2)));
        // Root split(1) is Horizontal (side-by-side) -> vertical divider, 1:1 -> 50.
        let root = nodes.iter().find(|n| n.pane_id == pid(1)).unwrap();
        assert_eq!(root.role, PaneAriaRole::Separator);
        assert_eq!(root.orientation, Some(PaneAriaOrientation::Vertical));
        assert_eq!(root.value_now, Some(50));
        // Split(3) is Vertical (stacked) -> horizontal divider, 3:1 -> 75.
        let inner = nodes.iter().find(|n| n.pane_id == pid(3)).unwrap();
        assert_eq!(inner.orientation, Some(PaneAriaOrientation::Horizontal));
        assert_eq!(inner.value_now, Some(75));
    }

    #[test]
    fn controller_drives_pointer_free_web_workflow() {
        fn run() -> (u64, Option<PaneId>, usize) {
            let mut tree = nested();
            let mut controller = PaneWebKeyboardController::new(Some(pid(2)));
            let area = Rect::new(0, 0, 80, 24);
            let solve = |t: &PaneTree| t.solve_layout(area).expect("solves");

            // Right arrow -> focus the top-right pane.
            let layout = solve(&tree);
            controller.handle_key(&key(KeyCode::Right, Modifiers::NONE), &mut tree, &layout);
            assert_eq!(controller.active(), Some(pid(4)));

            // 's' -> split the active pane (structural).
            let layout = solve(&tree);
            let before = tree.nodes().count();
            controller.handle_key(
                &key(KeyCode::Char('s'), Modifiers::NONE),
                &mut tree,
                &layout,
            );
            assert!(tree.nodes().count() > before);

            // 'f' then Escape -> maximize then restore.
            let layout = solve(&tree);
            controller.handle_key(
                &key(KeyCode::Char('f'), Modifiers::NONE),
                &mut tree,
                &layout,
            );
            assert!(controller.maximized().is_some());
            let layout = solve(&tree);
            controller.handle_key(&key(KeyCode::Escape, Modifiers::NONE), &mut tree, &layout);
            assert_eq!(controller.maximized(), None);

            // Accessibility tree stays consistent: one roving Tab stop.
            let aria = controller.accessibility_tree(&tree);
            let zeros = aria.iter().filter(|n| n.tabindex == 0).count();
            (tree.state_hash(), controller.active(), zeros)
        }

        let first = run();
        let second = run();
        assert_eq!(first, second, "web keyboard workflow must be deterministic");
        assert_eq!(first.2, 1, "roving invariant holds after the workflow");
    }

    #[test]
    fn unbound_key_does_not_touch_state() {
        let mut tree = nested();
        let mut controller = PaneWebKeyboardController::new(Some(pid(2)));
        let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
        let before = tree.state_hash();
        let out = controller.handle_key(
            &key(KeyCode::Char('q'), Modifiers::NONE),
            &mut tree,
            &layout,
        );
        assert_eq!(out, PaneWebKeyOutcome::Unbound);
        assert_eq!(tree.state_hash(), before);
        assert_eq!(controller.active(), Some(pid(2)));
    }

    #[test]
    fn controller_surfaces_announcements_and_coalesces_resize() {
        let mut tree = nested();
        let mut controller = PaneWebKeyboardController::new(Some(pid(2)));
        let area = Rect::new(0, 0, 80, 24);

        // Right arrow -> focus announcement for the live region.
        let layout = tree.solve_layout(area).expect("solves");
        controller.handle_key(&key(KeyCode::Right, Modifiers::NONE), &mut tree, &layout);
        assert_eq!(
            controller
                .take_announcement()
                .expect("focus announces")
                .text,
            "Focused pane right_top"
        );

        // A burst of resize key presses coalesces to a single announcement
        // reflecting the final value (non-spammy live region).
        controller.set_active(Some(pid(2)));
        for _ in 0..3 {
            let layout = tree.solve_layout(area).expect("solves");
            controller.handle_key(
                &key(KeyCode::Char('='), Modifiers::NONE),
                &mut tree,
                &layout,
            );
        }
        let spoken = controller.take_announcement().expect("resize announces");
        assert_eq!(
            spoken.category,
            ftui_layout::PaneAnnouncementCategory::Resize
        );
        // Only one announcement for the burst.
        assert!(controller.take_announcement().is_none());
    }

    #[test]
    fn web_preferences_do_not_alter_keyboard_semantics() {
        use ftui_layout::PaneAccessibilityPreferences;
        // Same browser key stream, no-modes vs all-modes controller: focus /
        // maximize / topology / announcements must be identical (a11y modes are
        // presentation-only, mirroring the terminal-host invariant).
        fn drive(
            prefs: PaneAccessibilityPreferences,
        ) -> (u64, Option<PaneId>, Option<PaneId>, Vec<String>) {
            let mut tree = nested();
            let mut controller =
                PaneWebKeyboardController::new(Some(pid(2))).with_preferences(prefs);
            let area = Rect::new(0, 0, 80, 24);
            let seq = [
                key(KeyCode::Char('n'), Modifiers::NONE), // focus next
                key(KeyCode::Down, Modifiers::NONE),      // focus directional
                key(KeyCode::Char('s'), Modifiers::NONE), // split
                key(KeyCode::Char('f'), Modifiers::NONE), // maximize
                key(KeyCode::Escape, Modifiers::NONE),    // restore
            ];
            let mut announcements = Vec::new();
            for k in seq {
                let layout = tree.solve_layout(area).expect("solves");
                controller.handle_key(&k, &mut tree, &layout);
                if let Some(a) = controller.take_announcement() {
                    announcements.push(a.text);
                }
            }
            (
                tree.state_hash(),
                controller.active(),
                controller.maximized(),
                announcements,
            )
        }
        let plain = drive(PaneAccessibilityPreferences::none());
        let full = drive(PaneAccessibilityPreferences::all());
        assert_eq!(plain, full, "a11y modes must not change pane semantics");
    }

    #[test]
    fn accessibility_dataset_reflects_preferences() {
        use ftui_layout::PaneAccessibilityPreferences;
        // Default: every data-attribute is false.
        let plain = PaneWebKeyboardController::new(None);
        assert_eq!(
            plain.accessibility_dataset(),
            [
                ("data-pane-reduced-motion", false),
                ("data-pane-high-contrast", false),
                ("data-pane-large-target", false),
            ]
        );
        // Each preference flips exactly its own attribute.
        let prefs = PaneAccessibilityPreferences::none()
            .with_high_contrast(true)
            .with_large_target(true);
        let controller = PaneWebKeyboardController::new(None).with_preferences(prefs);
        let ds = controller.accessibility_dataset();
        assert_eq!(ds[0], ("data-pane-reduced-motion", false));
        assert_eq!(ds[1], ("data-pane-high-contrast", true));
        assert_eq!(ds[2], ("data-pane-large-target", true));
    }

    #[test]
    fn web_affordance_motion_honors_reduced_motion() {
        use ftui_layout::PaneAccessibilityPreferences;
        let reduced = PaneWebKeyboardController::new(None)
            .with_preferences(PaneAccessibilityPreferences::none().with_reduced_motion(true));
        assert!(reduced.affordance_motion().reduced_motion);
        assert!(
            !PaneWebKeyboardController::new(None)
                .affordance_motion()
                .reduced_motion
        );
    }
}
