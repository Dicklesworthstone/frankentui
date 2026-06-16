//! Terminal keyboard bindings for the pane workspace (bd-21pbi.2).
//!
//! This is the terminal host's binding layer over the canonical, host-agnostic
//! command model in `ftui_layout::pane_command` (bd-21pbi.1). It provides:
//!
//! - [`key_to_pane_command`]: the default terminal keymap, mapping raw
//!   `KeyEvent`s to canonical [`PaneCommand`]s. It binds only modifier-qualified
//!   keys (and `Tab`/`BackTab`) so it coexists with application shortcuts and
//!   never steals unrelated plain keys.
//! - [`PaneKeyboardController`]: a reusable host helper that translates a key,
//!   resolves it against a live `PaneTree`, applies any structural operations,
//!   and tracks focus/maximize state — so pointer-free pane workflows are fully
//!   operable from a few lines of host glue.
//! - [`render_pane_focus_ring`]: a stable focus indicator (a distinct border on
//!   the active pane), generic over the render target so it works with both a
//!   `Frame` and a bare `Buffer`.
//! - [`pane_keyboard_hints`]: discoverable shortcut hints for a help panel.
//!
//! The keymap is the terminal half of the cross-host parity guarantee: it emits
//! the same `PaneCommand` stream a conformant web binding (bd-21pbi.3) would for
//! equivalent intent, so both hosts reach identical pane state.

use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_layout::{
    PaneAccessibilityPreferences, PaneAffordanceMotion, PaneAnnouncement, PaneAnnouncer,
    PaneCardinalDirection, PaneCommand, PaneCommandAcceleration, PaneCommandEffect,
    PaneCommandResolution, PaneFocusContext, PaneFocusOrdinal, PaneId, PaneLayout,
    PaneResizeDirection, PaneTree, Rect, SplitAxis, announce_command, resolve_pane_command,
};
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};
use ftui_style::{PaneAffordanceTheme, ResolvedTheme};

/// Translate a raw terminal key event into a canonical [`PaneCommand`].
///
/// Returns `None` for key release events and for any key not in the default
/// pane keymap (so the host passes those through to the application). The
/// keymap is intentionally modifier-qualified to avoid stealing plain keys:
///
/// | Key | Command |
/// |-----|---------|
/// | `Tab` / `Shift+Tab` | `FocusNext` / `FocusPrevious` |
/// | `Ctrl+Arrow` | `FocusDirectional` |
/// | `Ctrl+Shift+Arrow` | `FocusEdge` |
/// | `Alt+Arrow` | `MovePane` |
/// | `Alt++` / `Alt+-` | `ResizeStep` grow / shrink |
/// | `Alt+s` / `Alt+v` | `Split` horizontal / vertical |
/// | `Alt+w` | `Close` |
/// | `Alt+[` / `Alt+]` | `SwapPane` previous / next |
/// | `Alt+z` / `Alt+r` | `Maximize` / `Restore` |
///
/// `resize_units` is the snap-step count for `ResizeStep` (computed by the
/// caller via [`PaneCommandAcceleration`]); it is ignored for other commands.
#[must_use]
pub fn key_to_pane_command(key: &KeyEvent, resize_units: u16) -> Option<PaneCommand> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let m = key.modifiers;
    let ctrl = m.contains(Modifiers::CTRL);
    let alt = m.contains(Modifiers::ALT);
    let shift = m.contains(Modifiers::SHIFT);
    // Plain = no Ctrl/Alt/Super (Shift may still be present, e.g. for BackTab).
    let unmodified = !ctrl && !alt && !m.contains(Modifiers::SUPER);

    match key.code {
        KeyCode::Tab if unmodified && !shift => Some(PaneCommand::FocusNext),
        KeyCode::BackTab => Some(PaneCommand::FocusPrevious),

        KeyCode::Left if ctrl && shift => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Left)),
        KeyCode::Right if ctrl && shift => {
            Some(PaneCommand::FocusEdge(PaneCardinalDirection::Right))
        }
        KeyCode::Up if ctrl && shift => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Up)),
        KeyCode::Down if ctrl && shift => Some(PaneCommand::FocusEdge(PaneCardinalDirection::Down)),

        KeyCode::Left if ctrl => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Left)),
        KeyCode::Right if ctrl => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Right)),
        KeyCode::Up if ctrl => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Up)),
        KeyCode::Down if ctrl => Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Down)),

        KeyCode::Left if alt => Some(PaneCommand::MovePane(PaneCardinalDirection::Left)),
        KeyCode::Right if alt => Some(PaneCommand::MovePane(PaneCardinalDirection::Right)),
        KeyCode::Up if alt => Some(PaneCommand::MovePane(PaneCardinalDirection::Up)),
        KeyCode::Down if alt => Some(PaneCommand::MovePane(PaneCardinalDirection::Down)),

        KeyCode::Char('+' | '=') if alt => Some(PaneCommand::ResizeStep {
            direction: PaneResizeDirection::Increase,
            units: resize_units,
        }),
        KeyCode::Char('-' | '_') if alt => Some(PaneCommand::ResizeStep {
            direction: PaneResizeDirection::Decrease,
            units: resize_units,
        }),

        KeyCode::Char('s' | 'S') if alt => Some(PaneCommand::Split(SplitAxis::Horizontal)),
        KeyCode::Char('v' | 'V') if alt => Some(PaneCommand::Split(SplitAxis::Vertical)),
        KeyCode::Char('w' | 'W') if alt => Some(PaneCommand::Close),
        KeyCode::Char('[') if alt => Some(PaneCommand::SwapPane(PaneFocusOrdinal::Previous)),
        KeyCode::Char(']') if alt => Some(PaneCommand::SwapPane(PaneFocusOrdinal::Next)),
        KeyCode::Char('z' | 'Z') if alt => Some(PaneCommand::Maximize),
        KeyCode::Char('r' | 'R') if alt => Some(PaneCommand::Restore),

        _ => None,
    }
}

/// Outcome of feeding one key event to a [`PaneKeyboardController`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneKeyOutcome {
    /// The key is not a pane binding; the host should handle it normally.
    Unbound,
    /// The command was resolved (and any structural operations applied).
    Handled {
        /// The command the key mapped to.
        command: PaneCommand,
        /// The full resolution (effect + resulting focus state).
        resolution: PaneCommandResolution,
    },
    /// The command resolved to structural operations but one failed to apply
    /// (e.g. an invalid topology); focus state is left unchanged.
    Failed {
        /// The command that failed.
        command: PaneCommand,
    },
}

/// A reusable terminal pane keyboard host: translates keys to commands,
/// resolves them against a live tree, applies structural operations, and tracks
/// focus + maximize state. Hosts own the `PaneTree`; this owns the focus state
/// and a monotonic operation counter.
#[derive(Debug, Clone)]
pub struct PaneKeyboardController {
    focus: PaneFocusContext,
    acceleration: PaneCommandAcceleration,
    op_seed: u64,
    repeat_count: u16,
    announcer: PaneAnnouncer,
    preferences: PaneAccessibilityPreferences,
}

impl PaneKeyboardController {
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

    /// Take the pending accessibility announcement (for a status line / screen
    /// reader), coalesced and de-duplicated. Call once per render.
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

    /// Update the accessibility preferences at runtime (e.g. when the user
    /// toggles a mode). Never affects focus/command semantics — only the
    /// presentation helpers below.
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

    /// Build a focus ring for the active pane from `theme`, honoring the
    /// high-contrast preference. The ring color is contrast-clamped against the
    /// pane surface (AAA when high-contrast is on, AA otherwise).
    #[must_use]
    pub fn focus_ring(&self, theme: &ResolvedTheme) -> PaneFocusRing {
        let affordance = PaneAffordanceTheme::from_resolved(theme, self.preferences.high_contrast);
        PaneFocusRing::themed(&affordance)
    }

    /// The current focus context (active + maximized pane).
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

    /// Set the active pane (e.g. when a pointer click changes focus).
    pub const fn set_active(&mut self, active: Option<PaneId>) {
        self.focus.active = active;
    }

    /// Translate, resolve, and apply one key event. Structural commands mutate
    /// `tree`; focus/maximize commands update internal state. The `layout` must
    /// be the solved layout of `tree` (used for spatial focus + move).
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        tree: &mut PaneTree,
        layout: &PaneLayout,
    ) -> PaneKeyOutcome {
        // Sustained key repeat accelerates resize stepping.
        if key.kind == KeyEventKind::Repeat {
            self.repeat_count = self.repeat_count.saturating_add(1);
        } else {
            self.repeat_count = 0;
        }
        let resize_units = self.acceleration.units_for(self.repeat_count);

        let Some(command) = key_to_pane_command(key, resize_units) else {
            return PaneKeyOutcome::Unbound;
        };

        let resolution = resolve_pane_command(tree, layout, self.focus, command);
        if let PaneCommandEffect::Structural(ops) = &resolution.effect {
            for op in ops {
                if tree.apply_operation(self.op_seed, op.clone()).is_err() {
                    return PaneKeyOutcome::Failed { command };
                }
                self.op_seed += 1;
            }
        }
        self.focus.active = resolution.next_active;
        self.focus.maximized = resolution.next_maximized;
        self.announcer
            .offer(announce_command(command, &resolution, tree));
        PaneKeyOutcome::Handled {
            command,
            resolution,
        }
    }
}

/// A stable focus indicator: a distinct border drawn around the active pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneFocusRing {
    /// Border glyph set (default: double-line, visually distinct from pane
    /// content borders).
    pub border: BorderChars,
    /// Cell whose fg/bg/attrs style the border glyphs.
    pub cell: Cell,
}

impl Default for PaneFocusRing {
    fn default() -> Self {
        Self {
            border: BorderChars::DOUBLE,
            // Gold border: high-contrast and stable across themes. Used when no
            // theme is wired in; prefer [`PaneFocusRing::themed`] when a theme is
            // available so the ring respects palette + high-contrast preferences.
            cell: Cell::from_char(' ').with_fg(PackedRgba::rgb(0xFF, 0xD7, 0x00)),
        }
    }
}

impl PaneFocusRing {
    /// Build a focus ring whose color is the theme's contrast-guaranteed pane
    /// focus-ring affordance (bd-3bfbp).
    ///
    /// The supplied [`PaneAffordanceTheme`] has already been contrast-clamped
    /// against the pane surface (AA by default, AAA when built in the
    /// high-contrast profile), so the ring stays visible across every theme
    /// without per-call tuning.
    #[must_use]
    pub fn themed(affordance: &PaneAffordanceTheme) -> Self {
        let rgb = affordance.focus_ring.to_rgb();
        Self {
            border: BorderChars::DOUBLE,
            cell: Cell::from_char(' ').with_fg(PackedRgba::rgb(rgb.r, rgb.g, rgb.b)),
        }
    }
}

/// Draw a focus ring around `rect` on any render target (`Frame` or `Buffer`).
///
/// Visually stable: it draws a deterministic border and never depends on
/// animation or timing, so the active pane is unambiguous after any command.
pub fn render_pane_focus_ring<T: Draw>(target: &mut T, rect: Rect, ring: &PaneFocusRing) {
    target.draw_border(rect, ring.border, ring.cell);
}

/// One discoverable keyboard hint for a help panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneKeyHint {
    /// Human-readable key combination, e.g. `"Ctrl+Arrows"`.
    pub keys: &'static str,
    /// What the binding does.
    pub action: &'static str,
}

/// The canonical, discoverable pane keyboard shortcut hints, in display order.
#[must_use]
pub fn pane_keyboard_hints() -> &'static [PaneKeyHint] {
    &[
        PaneKeyHint {
            keys: "Tab / Shift+Tab",
            action: "focus next / previous pane",
        },
        PaneKeyHint {
            keys: "Ctrl+Arrows",
            action: "focus pane by direction",
        },
        PaneKeyHint {
            keys: "Ctrl+Shift+Arrows",
            action: "focus pane at edge",
        },
        PaneKeyHint {
            keys: "Alt+Arrows",
            action: "move pane",
        },
        PaneKeyHint {
            keys: "Alt++ / Alt+-",
            action: "grow / shrink pane",
        },
        PaneKeyHint {
            keys: "Alt+s / Alt+v",
            action: "split horizontal / vertical",
        },
        PaneKeyHint {
            keys: "Alt+w",
            action: "close pane",
        },
        PaneKeyHint {
            keys: "Alt+[ / Alt+]",
            action: "swap with previous / next pane",
        },
        PaneKeyHint {
            keys: "Alt+z / Alt+r",
            action: "maximize / restore pane",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_layout::{
        PANE_TREE_SCHEMA_VERSION, PaneLeaf, PaneNodeRecord, PaneSplit, PaneSplitRatio,
        PaneTreeSnapshot,
    };
    use ftui_render::buffer::Buffer;
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
                        ratio: PaneSplitRatio::new(1, 1).unwrap(),
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
    fn keymap_covers_the_full_vocabulary() {
        let none = Modifiers::NONE;
        let ctrl = Modifiers::CTRL;
        let alt = Modifiers::ALT;
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;

        assert_eq!(
            key_to_pane_command(&key(KeyCode::Tab, none), 1),
            Some(PaneCommand::FocusNext)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::BackTab, Modifiers::SHIFT), 1),
            Some(PaneCommand::FocusPrevious)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Left, ctrl), 1),
            Some(PaneCommand::FocusDirectional(PaneCardinalDirection::Left))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Down, ctrl_shift), 1),
            Some(PaneCommand::FocusEdge(PaneCardinalDirection::Down))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Right, alt), 1),
            Some(PaneCommand::MovePane(PaneCardinalDirection::Right))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('+'), alt), 4),
            Some(PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Increase,
                units: 4
            })
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('-'), alt), 1),
            Some(PaneCommand::ResizeStep {
                direction: PaneResizeDirection::Decrease,
                units: 1
            })
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('s'), alt), 1),
            Some(PaneCommand::Split(SplitAxis::Horizontal))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('v'), alt), 1),
            Some(PaneCommand::Split(SplitAxis::Vertical))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('w'), alt), 1),
            Some(PaneCommand::Close)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('['), alt), 1),
            Some(PaneCommand::SwapPane(PaneFocusOrdinal::Previous))
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('z'), alt), 1),
            Some(PaneCommand::Maximize)
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('r'), alt), 1),
            Some(PaneCommand::Restore)
        );
    }

    #[test]
    fn keymap_does_not_steal_plain_or_app_keys() {
        // Plain letters and plain arrows are left for the application.
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('a'), Modifiers::NONE), 1),
            None
        );
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Left, Modifiers::NONE), 1),
            None
        );
        // Ctrl+C (common app binding) is not a pane command.
        assert_eq!(
            key_to_pane_command(&key(KeyCode::Char('c'), Modifiers::CTRL), 1),
            None
        );
        // Release events are ignored.
        let mut release = key(KeyCode::Tab, Modifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_to_pane_command(&release, 1), None);
    }

    #[test]
    fn controller_drives_pointer_free_workflow() {
        // A keyboard-only session: focus, split, focus by direction, close,
        // maximize/restore — no pointer events at all.
        fn run() -> (u64, Option<PaneId>, Option<PaneId>) {
            let mut tree = nested();
            let mut controller = PaneKeyboardController::new(Some(pid(2)));
            let area = Rect::new(0, 0, 80, 24);

            let solve = |t: &PaneTree| t.solve_layout(area).expect("solves");

            // Tab -> focus next (2 -> 4).
            let layout = solve(&tree);
            let out =
                controller.handle_key(&key(KeyCode::Tab, Modifiers::NONE), &mut tree, &layout);
            assert!(matches!(out, PaneKeyOutcome::Handled { .. }));
            assert_eq!(controller.active(), Some(pid(4)));

            // Ctrl+Down -> focus bottom (4 -> 5).
            let layout = solve(&tree);
            controller.handle_key(&key(KeyCode::Down, Modifiers::CTRL), &mut tree, &layout);
            assert_eq!(controller.active(), Some(pid(5)));

            // Alt+s -> split the active leaf (structural; tree grows).
            let layout = solve(&tree);
            let before = tree.nodes().count();
            controller.handle_key(&key(KeyCode::Char('s'), Modifiers::ALT), &mut tree, &layout);
            assert!(tree.nodes().count() > before, "split must grow the tree");

            // Alt+z then Alt+r -> maximize then restore.
            let layout = solve(&tree);
            controller.handle_key(&key(KeyCode::Char('z'), Modifiers::ALT), &mut tree, &layout);
            assert!(controller.maximized().is_some());
            let layout = solve(&tree);
            controller.handle_key(&key(KeyCode::Char('r'), Modifiers::ALT), &mut tree, &layout);
            assert_eq!(controller.maximized(), None);

            (
                tree.state_hash(),
                controller.active(),
                controller.maximized(),
            )
        }

        let first = run();
        let second = run();
        assert_eq!(
            first, second,
            "keyboard-only workflow must be deterministic"
        );
    }

    #[test]
    fn controller_close_updates_focus_to_survivor() {
        let mut tree = nested();
        let mut controller = PaneKeyboardController::new(Some(pid(2)));
        let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
        let out =
            controller.handle_key(&key(KeyCode::Char('w'), Modifiers::ALT), &mut tree, &layout);
        match out {
            PaneKeyOutcome::Handled { command, .. } => {
                assert_eq!(command, PaneCommand::Close);
            }
            other => panic!("expected handled close, got {other:?}"),
        }
        // Active pane must now be a surviving leaf, not the closed one.
        assert_ne!(controller.active(), Some(pid(2)));
        assert!(controller.active().is_some());
    }

    #[test]
    fn unbound_key_does_not_touch_state() {
        let mut tree = nested();
        let mut controller = PaneKeyboardController::new(Some(pid(2)));
        let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
        let before = tree.state_hash();
        let out = controller.handle_key(
            &key(KeyCode::Char('q'), Modifiers::NONE),
            &mut tree,
            &layout,
        );
        assert_eq!(out, PaneKeyOutcome::Unbound);
        assert_eq!(controller.active(), Some(pid(2)));
        assert_eq!(tree.state_hash(), before);
    }

    #[test]
    fn repeat_accelerates_resize_units() {
        let mut tree = nested();
        // Make left(2) the active pane so its enclosing split is the root.
        let mut controller = PaneKeyboardController::new(Some(pid(2)));
        let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
        let mut repeat = key(KeyCode::Char('+'), Modifiers::ALT);
        repeat.kind = KeyEventKind::Repeat;
        // Drive enough repeats to cross the acceleration threshold (default 3).
        let mut last_units = 0u16;
        for _ in 0..4 {
            if let PaneKeyOutcome::Handled {
                command: PaneCommand::ResizeStep { units, .. },
                ..
            } = controller.handle_key(&repeat, &mut tree, &layout)
            {
                last_units = units;
            }
        }
        assert_eq!(
            last_units,
            PaneCommandAcceleration::default().accelerated_units
        );
    }

    #[test]
    fn focus_ring_draws_a_distinct_border() {
        let mut buffer = Buffer::new(10, 6);
        render_pane_focus_ring(
            &mut buffer,
            Rect::new(0, 0, 6, 4),
            &PaneFocusRing::default(),
        );
        // Double-line top-left corner glyph.
        assert_eq!(
            buffer.get(0, 0).and_then(|c| c.content.as_char()),
            Some('╔')
        );
        assert_eq!(
            buffer.get(5, 3).and_then(|c| c.content.as_char()),
            Some('╝')
        );
    }

    #[test]
    fn themed_focus_ring_uses_the_affordance_color() {
        use ftui_style::theme::themes;
        let resolved = themes::dark().resolve(true);
        let affordance = PaneAffordanceTheme::from_resolved(&resolved, false);
        let ring = PaneFocusRing::themed(&affordance);
        // The ring's fg must equal the theme's contrast-clamped focus-ring color,
        // not the hardcoded gold default.
        let want = affordance.focus_ring.to_rgb();
        assert_eq!(ring.cell.fg, PackedRgba::rgb(want.r, want.g, want.b));
        assert_ne!(ring.cell.fg, PaneFocusRing::default().cell.fg);
        // Glyph set is unchanged (still the distinct double-line border).
        assert_eq!(ring.border, BorderChars::DOUBLE);
    }

    #[test]
    fn themed_focus_ring_high_contrast_is_at_least_as_visible() {
        use ftui_style::theme::themes;
        let resolved = themes::solarized_light().resolve(false);
        let normal = PaneAffordanceTheme::from_resolved(&resolved, false);
        let high = PaneAffordanceTheme::from_resolved(&resolved, true);
        let ratio = |c: ftui_style::color::Color| {
            ftui_style::color::contrast_ratio(c.to_rgb(), resolved.surface.to_rgb())
        };
        assert!(ratio(high.focus_ring) + 1e-9 >= ratio(normal.focus_ring));
    }

    #[test]
    fn preferences_do_not_alter_keyboard_semantics() {
        use ftui_layout::PaneAccessibilityPreferences;
        // Identical key stream, two controllers: one with no a11y modes, one with
        // all of them. Focus/maximize/topology and announcements must be byte
        // identical — a11y modes are presentation-only.
        fn drive(
            prefs: PaneAccessibilityPreferences,
        ) -> (u64, Option<PaneId>, Option<PaneId>, Vec<String>) {
            let mut tree = nested();
            let mut controller = PaneKeyboardController::new(Some(pid(2))).with_preferences(prefs);
            let area = Rect::new(0, 0, 80, 24);
            let seq = [
                key(KeyCode::Tab, Modifiers::NONE),
                key(KeyCode::Down, Modifiers::CTRL),
                key(KeyCode::Char('s'), Modifiers::ALT),
                key(KeyCode::Char('z'), Modifiers::ALT),
                key(KeyCode::Char('r'), Modifiers::ALT),
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
    fn focus_ring_honors_high_contrast_preference() {
        use ftui_layout::PaneAccessibilityPreferences;
        use ftui_style::theme::themes;
        let theme = themes::dark().resolve(true);
        let normal_aff = PaneAffordanceTheme::from_resolved(&theme, false);
        let high_aff = PaneAffordanceTheme::from_resolved(&theme, true);

        // The controller wires its high-contrast preference straight into the
        // themed ring: each ring's color matches the affordance for its profile.
        let normal_ctl = PaneKeyboardController::new(Some(pid(2)));
        let high_ctl = PaneKeyboardController::new(Some(pid(2)))
            .with_preferences(PaneAccessibilityPreferences::none().with_high_contrast(true));
        assert_eq!(
            normal_ctl.focus_ring(&theme).cell.fg,
            PaneFocusRing::themed(&normal_aff).cell.fg
        );
        assert_eq!(
            high_ctl.focus_ring(&theme).cell.fg,
            PaneFocusRing::themed(&high_aff).cell.fg
        );

        // And high contrast is never less visible than the default profile.
        let ratio = |c: ftui_style::color::Color| {
            ftui_style::color::contrast_ratio(c.to_rgb(), theme.surface.to_rgb())
        };
        assert!(ratio(high_aff.focus_ring) + 1e-9 >= ratio(normal_aff.focus_ring));
    }

    #[test]
    fn controller_affordance_motion_honors_reduced_motion() {
        use ftui_layout::PaneAccessibilityPreferences;
        let reduced = PaneKeyboardController::new(None)
            .with_preferences(PaneAccessibilityPreferences::none().with_reduced_motion(true));
        assert!(reduced.affordance_motion().reduced_motion);
        let plain = PaneKeyboardController::new(None);
        assert!(!plain.affordance_motion().reduced_motion);
    }

    #[test]
    fn hints_cover_every_binding_family() {
        let hints = pane_keyboard_hints();
        assert_eq!(hints.len(), 9);
        assert!(hints.iter().any(|h| h.action.contains("focus next")));
        assert!(hints.iter().any(|h| h.action.contains("split")));
        assert!(hints.iter().any(|h| h.action.contains("maximize")));
    }

    #[test]
    fn controller_surfaces_announcements() {
        let mut tree = nested();
        let mut controller = PaneKeyboardController::new(Some(pid(2)));
        let layout = tree.solve_layout(Rect::new(0, 0, 80, 24)).expect("solves");
        // Tab -> focus next (2 -> 4) announces the newly focused pane.
        controller.handle_key(&key(KeyCode::Tab, Modifiers::NONE), &mut tree, &layout);
        let spoken = controller.take_announcement().expect("focus announces");
        assert_eq!(spoken.text, "Focused pane right_top");
        // Nothing pending after taking.
        assert!(controller.take_announcement().is_none());
    }
}
