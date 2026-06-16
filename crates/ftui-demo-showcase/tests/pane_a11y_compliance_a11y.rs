#![forbid(unsafe_code)]

//! Pane accessibility compliance + assistive-tech regression suite (bd-21pbi.6).
//!
//! This is the executable, repeatable form of the a11y compliance matrix at
//! `tests/e2e/pane_a11y_compliance_matrix.json`. It drives the **production**
//! host controllers — `ftui_runtime::pane_keymap::PaneKeyboardController`
//! (terminal) and `ftui_web::pane_keyboard::PaneWebKeyboardController` (web) —
//! through every compliance dimension and proves regressions are caught before
//! release:
//!
//! | Dimension | What it proves |
//! |-----------|----------------|
//! | Keyboard parity | Every pane op is reachable pointer-free; equivalent intent reaches identical state on both hosts |
//! | Focus visibility | Contrast-safe focus ring (terminal); roving-tabindex + separator ARIA (web) |
//! | Announcements | Every effective command announces; bursts coalesce |
//! | Adaptive modes | Reduced-motion / high-contrast / large-target are honored and presentation-only |
//!
//! Each scenario emits a JSONL diagnostic line including the host, the command
//! sequence, and the resulting focus state, so a CI failure is triageable from
//! the artifact alone (bd-21pbi.6 AC#4).
//!
//! The final test parses the matrix JSON and asserts every named evidence test
//! exists in this file, so the matrix and the suite cannot drift.
//!
//! Run: `cargo test -p ftui-demo-showcase --test pane_a11y_compliance_a11y`

use std::collections::BTreeMap;
use std::path::PathBuf;

use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_layout::{
    PANE_TREE_SCHEMA_VERSION, PaneAccessibilityPreferences, PaneAnnouncementCategory, PaneId,
    PaneLeaf, PaneNodeRecord, PaneSplit, PaneSplitRatio, PaneTree, PaneTreeSnapshot, Rect,
    SplitAxis,
};
use ftui_runtime::pane_keymap::{PaneFocusRing, PaneKeyOutcome, PaneKeyboardController};
use ftui_style::PaneAffordanceTheme;
use ftui_style::color::{WCAG_AA_LARGE_TEXT, contrast_ratio};
use ftui_style::theme::themes;
use ftui_web::pane_keyboard::{PaneAriaRole, PaneWebKeyOutcome, PaneWebKeyboardController};

// =============================================================================
// Diagnostics (bd-21pbi.6 AC#4: focus state + command sequence + host context)
// =============================================================================

/// Emit a structured JSONL diagnostic for CI artifact review.
fn log_evidence(host: &str, scenario: &str, commands: &[&str], focus: &str, passed: bool) {
    let cmds = commands.join(" -> ");
    eprintln!(
        "{{\"suite\":\"pane_a11y_compliance\",\"host\":\"{host}\",\"scenario\":\"{scenario}\",\
\"commands\":\"{cmds}\",\"focus\":\"{focus}\",\"passed\":{passed}}}"
    );
}

fn focus_desc(active: Option<PaneId>, maximized: Option<PaneId>) -> String {
    format!(
        "active={} maximized={}",
        active.map_or(0, PaneId::get),
        maximized.map_or(0, PaneId::get)
    )
}

// =============================================================================
// Fixtures
// =============================================================================

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

const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

// =============================================================================
// Dimension 1 — Keyboard parity
// =============================================================================

#[test]
fn terminal_keyboard_completes_all_pane_ops() {
    let mut tree = nested();
    let mut ctl = PaneKeyboardController::new(Some(pid(2)));
    let mut commands: Vec<&str> = Vec::new();

    // Focus navigation, split, maximize, restore, close — all pointer-free.
    let steps: [(KeyEvent, &str); 5] = [
        (key(KeyCode::Tab, Modifiers::NONE), "FocusNext"),
        (key(KeyCode::Char('s'), Modifiers::ALT), "Split(H)"),
        (key(KeyCode::Char('z'), Modifiers::ALT), "Maximize"),
        (key(KeyCode::Char('r'), Modifiers::ALT), "Restore"),
        (key(KeyCode::Char('w'), Modifiers::ALT), "Close"),
    ];
    for (k, label) in steps {
        let layout = tree.solve_layout(AREA).expect("solves");
        let outcome = ctl.handle_key(&k, &mut tree, &layout);
        assert!(
            matches!(outcome, PaneKeyOutcome::Handled { .. }),
            "{label} must be handled, got {outcome:?}"
        );
        commands.push(label);
    }
    assert!(
        ctl.active().is_some(),
        "a pane stays focused after the workflow"
    );
    log_evidence(
        "terminal",
        "keyboard_completes_all_pane_ops",
        &commands,
        &focus_desc(ctl.active(), ctl.maximized()),
        true,
    );
}

#[test]
fn web_keyboard_completes_all_pane_ops() {
    let mut tree = nested();
    let mut ctl = PaneWebKeyboardController::new(Some(pid(2)));
    let mut commands: Vec<&str> = Vec::new();

    let steps: [(KeyEvent, &str); 5] = [
        (key(KeyCode::Char('n'), Modifiers::NONE), "FocusNext"),
        (key(KeyCode::Char('s'), Modifiers::NONE), "Split(H)"),
        (key(KeyCode::Char('f'), Modifiers::NONE), "Maximize"),
        (key(KeyCode::Escape, Modifiers::NONE), "Restore"),
        (key(KeyCode::Char('x'), Modifiers::NONE), "Close"),
    ];
    for (k, label) in steps {
        let layout = tree.solve_layout(AREA).expect("solves");
        let outcome = ctl.handle_key(&k, &mut tree, &layout);
        assert!(
            matches!(outcome, PaneWebKeyOutcome::Handled { .. }),
            "{label} must be handled, got {outcome:?}"
        );
        commands.push(label);
    }
    assert!(
        ctl.active().is_some(),
        "a pane stays focused after the workflow"
    );
    log_evidence(
        "web",
        "keyboard_completes_all_pane_ops",
        &commands,
        &focus_desc(ctl.active(), ctl.maximized()),
        true,
    );
}

#[test]
fn web_keymap_is_browser_safe() {
    // The web host must never consume Ctrl/Super chords (browser/OS reserved).
    let mut tree = nested();
    let mut ctl = PaneWebKeyboardController::new(Some(pid(2)));
    for code in [
        KeyCode::Char('w'),
        KeyCode::Char('t'),
        KeyCode::Char('='),
        KeyCode::Left,
    ] {
        for m in [
            Modifiers::CTRL,
            Modifiers::SUPER,
            Modifiers::CTRL | Modifiers::SHIFT,
        ] {
            let layout = tree.solve_layout(AREA).expect("solves");
            let before = tree.state_hash();
            let outcome = ctl.handle_key(&key(code, m), &mut tree, &layout);
            assert!(
                matches!(outcome, PaneWebKeyOutcome::Unbound),
                "reserved chord must not be consumed: {code:?}+{m:?}"
            );
            assert_eq!(
                tree.state_hash(),
                before,
                "reserved chord must not mutate the tree"
            );
        }
    }
    log_evidence(
        "web",
        "keymap_is_browser_safe",
        &["Ctrl/Super refused"],
        "unchanged",
        true,
    );
}

#[test]
fn cross_host_command_parity() {
    // Equivalent intent, host-native keys: both hosts must reach byte-identical
    // pane state (topology hash + active + maximized). The keymaps differ; the
    // resulting state must not.
    fn run_terminal() -> (u64, Option<PaneId>, Option<PaneId>) {
        let mut tree = nested();
        let mut ctl = PaneKeyboardController::new(Some(pid(2)));
        for k in [
            key(KeyCode::Tab, Modifiers::NONE),      // FocusNext
            key(KeyCode::Char('s'), Modifiers::ALT), // Split(H)
            key(KeyCode::Char('z'), Modifiers::ALT), // Maximize
            key(KeyCode::Char('r'), Modifiers::ALT), // Restore
        ] {
            let layout = tree.solve_layout(AREA).expect("solves");
            ctl.handle_key(&k, &mut tree, &layout);
        }
        (tree.state_hash(), ctl.active(), ctl.maximized())
    }
    fn run_web() -> (u64, Option<PaneId>, Option<PaneId>) {
        let mut tree = nested();
        let mut ctl = PaneWebKeyboardController::new(Some(pid(2)));
        for k in [
            key(KeyCode::Char('n'), Modifiers::NONE), // FocusNext
            key(KeyCode::Char('s'), Modifiers::NONE), // Split(H)
            key(KeyCode::Char('f'), Modifiers::NONE), // Maximize
            key(KeyCode::Escape, Modifiers::NONE),    // Restore
        ] {
            let layout = tree.solve_layout(AREA).expect("solves");
            ctl.handle_key(&k, &mut tree, &layout);
        }
        (tree.state_hash(), ctl.active(), ctl.maximized())
    }
    let term = run_terminal();
    let web = run_web();
    assert_eq!(
        term, web,
        "equivalent intent must reach identical state across hosts"
    );
    log_evidence(
        "both",
        "cross_host_command_parity",
        &["FocusNext", "Split(H)", "Maximize", "Restore"],
        &focus_desc(term.1, term.2),
        true,
    );
}

// =============================================================================
// Dimension 2 — Focus visibility
// =============================================================================

#[test]
fn terminal_focus_ring_is_contrast_safe() {
    let resolved = themes::dark().resolve(true);
    let ctl = PaneKeyboardController::new(Some(pid(2)));
    let affordance = PaneAffordanceTheme::from_resolved(&resolved, false);

    // The controller wires its (default) preference straight into the ring.
    assert_eq!(
        ctl.focus_ring(&resolved).cell.fg,
        PaneFocusRing::themed(&affordance).cell.fg
    );
    // The ring color is contrast-safe against the pane surface.
    let ratio = contrast_ratio(affordance.focus_ring.to_rgb(), resolved.surface.to_rgb());
    assert!(
        ratio >= WCAG_AA_LARGE_TEXT,
        "focus ring contrast {ratio:.2} is below the large-text floor"
    );
    log_evidence(
        "terminal",
        "focus_ring_is_contrast_safe",
        &["focus"],
        "active=2",
        true,
    );
}

#[test]
fn web_roving_tabindex_invariant() {
    let tree = nested();
    let ctl = PaneWebKeyboardController::new(Some(pid(4)));
    let aria = ctl.accessibility_tree(&tree);

    // Exactly one leaf carries tabindex == 0 (the active pane); others are -1.
    let zero_leaves: Vec<_> = aria
        .iter()
        .filter(|n| n.role == PaneAriaRole::Group && n.tabindex == 0)
        .collect();
    assert_eq!(zero_leaves.len(), 1, "exactly one leaf is tab-focusable");
    assert_eq!(
        zero_leaves[0].pane_id,
        pid(4),
        "the active pane is the focusable one"
    );
    assert!(
        zero_leaves[0].current,
        "the active pane carries aria-current"
    );
    // Every other leaf is -1.
    for n in aria
        .iter()
        .filter(|n| n.role == PaneAriaRole::Group && n.pane_id != pid(4))
    {
        assert_eq!(n.tabindex, -1);
        assert!(!n.current);
    }
    log_evidence(
        "web",
        "roving_tabindex_invariant",
        &["accessibility_tree"],
        "active=4",
        true,
    );
}

#[test]
fn web_separator_aria_semantics() {
    let tree = nested();
    let ctl = PaneWebKeyboardController::new(Some(pid(2)));
    let aria = ctl.accessibility_tree(&tree);

    let separators: Vec<_> = aria
        .iter()
        .filter(|n| n.role == PaneAriaRole::Separator)
        .collect();
    assert!(!separators.is_empty(), "split nodes expose separators");
    for sep in separators {
        assert!(
            sep.orientation.is_some(),
            "separator exposes aria-orientation"
        );
        let value = sep.value_now.expect("separator exposes aria-valuenow");
        assert!(
            value <= 100,
            "valuenow is a 0..=100 percentage, got {value}"
        );
        assert_eq!(sep.value_min, Some(0));
        assert_eq!(sep.value_max, Some(100));
        assert_eq!(
            sep.tabindex, -1,
            "separators are described, not tab-focused"
        );
    }
    log_evidence(
        "web",
        "separator_aria_semantics",
        &["accessibility_tree"],
        "active=2",
        true,
    );
}

// =============================================================================
// Dimension 3 — Announcements
// =============================================================================

#[test]
fn terminal_every_command_announces() {
    let mut tree = nested();
    let mut ctl = PaneKeyboardController::new(Some(pid(2)));
    for (k, label) in [
        (key(KeyCode::Tab, Modifiers::NONE), "FocusNext"),
        (key(KeyCode::Char('s'), Modifiers::ALT), "Split"),
        (key(KeyCode::Char('z'), Modifiers::ALT), "Maximize"),
    ] {
        let layout = tree.solve_layout(AREA).expect("solves");
        ctl.handle_key(&k, &mut tree, &layout);
        let announcement = ctl.take_announcement();
        assert!(
            announcement.is_some(),
            "{label} must produce an announcement"
        );
        log_evidence(
            "terminal",
            "every_command_announces",
            &[label],
            &announcement.unwrap().text,
            true,
        );
    }
}

#[test]
fn web_every_command_announces() {
    let mut tree = nested();
    let mut ctl = PaneWebKeyboardController::new(Some(pid(2)));
    let layout = tree.solve_layout(AREA).expect("solves");
    ctl.handle_key(
        &key(KeyCode::Char('n'), Modifiers::NONE),
        &mut tree,
        &layout,
    );
    let announcement = ctl.take_announcement().expect("focus announces");
    assert!(announcement.text.starts_with("Focused pane"));
    log_evidence(
        "web",
        "every_command_announces",
        &["FocusNext"],
        &announcement.text,
        true,
    );
}

#[test]
fn announcements_coalesce() {
    // A burst of resize key presses must coalesce to a single announcement
    // reflecting the final value (non-spammy live region).
    let mut tree = nested();
    let mut ctl = PaneWebKeyboardController::new(Some(pid(2)));
    for _ in 0..4 {
        let layout = tree.solve_layout(AREA).expect("solves");
        ctl.handle_key(
            &key(KeyCode::Char('='), Modifiers::NONE),
            &mut tree,
            &layout,
        );
    }
    let spoken = ctl.take_announcement().expect("resize announces once");
    assert_eq!(spoken.category, PaneAnnouncementCategory::Resize);
    assert!(
        ctl.take_announcement().is_none(),
        "burst coalesces to one announcement"
    );
    log_evidence(
        "web",
        "announcements_coalesce",
        &["Resize x4"],
        &spoken.text,
        true,
    );
}

// =============================================================================
// Dimension 4 — Adaptive modes
// =============================================================================

#[test]
fn adaptive_modes_preserve_semantics() {
    // Presentation-only contract: the same key stream produces identical
    // topology/focus/maximize/announcements with no modes vs all modes, on BOTH
    // hosts.
    fn drive_terminal(
        prefs: PaneAccessibilityPreferences,
    ) -> (u64, Option<PaneId>, Option<PaneId>, Vec<String>) {
        let mut tree = nested();
        let mut ctl = PaneKeyboardController::new(Some(pid(2))).with_preferences(prefs);
        let mut announcements = Vec::new();
        for k in [
            key(KeyCode::Tab, Modifiers::NONE),
            key(KeyCode::Down, Modifiers::CTRL),
            key(KeyCode::Char('s'), Modifiers::ALT),
            key(KeyCode::Char('z'), Modifiers::ALT),
            key(KeyCode::Char('r'), Modifiers::ALT),
        ] {
            let layout = tree.solve_layout(AREA).expect("solves");
            ctl.handle_key(&k, &mut tree, &layout);
            if let Some(a) = ctl.take_announcement() {
                announcements.push(a.text);
            }
        }
        (
            tree.state_hash(),
            ctl.active(),
            ctl.maximized(),
            announcements,
        )
    }
    fn drive_web(
        prefs: PaneAccessibilityPreferences,
    ) -> (u64, Option<PaneId>, Option<PaneId>, Vec<String>) {
        let mut tree = nested();
        let mut ctl = PaneWebKeyboardController::new(Some(pid(2))).with_preferences(prefs);
        let mut announcements = Vec::new();
        for k in [
            key(KeyCode::Char('n'), Modifiers::NONE),
            key(KeyCode::Down, Modifiers::NONE),
            key(KeyCode::Char('s'), Modifiers::NONE),
            key(KeyCode::Char('f'), Modifiers::NONE),
            key(KeyCode::Escape, Modifiers::NONE),
        ] {
            let layout = tree.solve_layout(AREA).expect("solves");
            ctl.handle_key(&k, &mut tree, &layout);
            if let Some(a) = ctl.take_announcement() {
                announcements.push(a.text);
            }
        }
        (
            tree.state_hash(),
            ctl.active(),
            ctl.maximized(),
            announcements,
        )
    }

    let term = (
        drive_terminal(PaneAccessibilityPreferences::none()),
        drive_terminal(PaneAccessibilityPreferences::all()),
    );
    assert_eq!(
        term.0, term.1,
        "terminal: a11y modes must not change semantics"
    );

    let web = (
        drive_web(PaneAccessibilityPreferences::none()),
        drive_web(PaneAccessibilityPreferences::all()),
    );
    assert_eq!(web.0, web.1, "web: a11y modes must not change semantics");

    log_evidence(
        "both",
        "adaptive_modes_preserve_semantics",
        &["none == all"],
        &focus_desc(term.0.1, term.0.2),
        true,
    );
}

#[test]
fn reduced_motion_steps_instantly() {
    let reduced = PaneKeyboardController::new(None)
        .with_preferences(PaneAccessibilityPreferences::none().with_reduced_motion(true));
    let motion = reduced.affordance_motion();
    assert!(motion.reduced_motion);
    // Hover emphasis is full on the very first frame (stepped, no ramp).
    assert_eq!(
        motion.hover_emphasis_bps(0),
        ftui_layout::PANE_AFFORDANCE_EMPHASIS_FULL_BPS
    );
    // The default (animated) controller ramps up instead.
    let animated = PaneKeyboardController::new(None).affordance_motion();
    assert!(animated.hover_emphasis_bps(0) < ftui_layout::PANE_AFFORDANCE_EMPHASIS_FULL_BPS);
    log_evidence(
        "terminal",
        "reduced_motion_steps_instantly",
        &["hover@0"],
        "full",
        true,
    );
}

#[test]
fn high_contrast_lifts_focus_ring() {
    let resolved = themes::solarized_light().resolve(false);
    let normal = PaneAffordanceTheme::from_resolved(&resolved, false);
    let high = PaneAffordanceTheme::from_resolved(&resolved, true);
    let ratio = |c: ftui_style::color::Color| contrast_ratio(c.to_rgb(), resolved.surface.to_rgb());
    assert!(
        ratio(high.focus_ring) + 1e-9 >= ratio(normal.focus_ring),
        "high contrast must not reduce focus-ring contrast"
    );
    log_evidence(
        "terminal",
        "high_contrast_lifts_focus_ring",
        &["focus_ring"],
        "AAA",
        true,
    );
}

#[test]
fn large_target_enlarges_handles() {
    let normal = PaneAccessibilityPreferences::none();
    let large = PaneAccessibilityPreferences::none().with_large_target(true);
    for base in 1..=4u16 {
        assert_eq!(
            normal.enlarge_target(base),
            base,
            "default mode is identity"
        );
        assert!(
            large.enlarge_target(base) > base,
            "large target grows base {base}"
        );
    }
    log_evidence(
        "both",
        "large_target_enlarges_handles",
        &["enlarge_target"],
        "grown",
        true,
    );
}

#[test]
fn web_exposes_a11y_dataset() {
    let prefs = PaneAccessibilityPreferences::all();
    let ctl = PaneWebKeyboardController::new(None).with_preferences(prefs);
    let ds = ctl.accessibility_dataset();
    assert_eq!(ds[0], ("data-pane-reduced-motion", true));
    assert_eq!(ds[1], ("data-pane-high-contrast", true));
    assert_eq!(ds[2], ("data-pane-large-target", true));
    log_evidence(
        "web",
        "exposes_a11y_dataset",
        &["dataset"],
        "all=true",
        true,
    );
}

// =============================================================================
// Matrix ↔ suite consistency
// =============================================================================

#[test]
fn compliance_matrix_is_consistent_with_suite() {
    let matrix_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/e2e/pane_a11y_compliance_matrix.json");
    let raw = std::fs::read_to_string(&matrix_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", matrix_path.display()));
    let matrix: serde_json::Value = serde_json::from_str(&raw).expect("matrix is valid JSON");

    assert_eq!(matrix["schema_version"], "pane-a11y-compliance-v1");
    assert_eq!(matrix["matrix_bead"], "bd-21pbi.6");
    let hosts = matrix["hosts"].as_array().expect("hosts array");
    assert_eq!(hosts.len(), 2, "terminal + web hosts");

    let dimensions = matrix["dimensions"].as_array().expect("dimensions array");
    assert_eq!(dimensions.len(), 4, "four compliance dimensions");
    assert!(
        !matrix["known_limitations"]
            .as_array()
            .expect("known_limitations array")
            .is_empty(),
        "known limitations must be recorded with mitigations"
    );

    // Collect every evidence test name claimed by the matrix.
    let mut evidence: Vec<String> = Vec::new();
    for dim in dimensions {
        for key in ["terminal_evidence", "web_evidence"] {
            for name in dim[key].as_array().into_iter().flatten() {
                evidence.push(
                    name.as_str()
                        .expect("evidence name is a string")
                        .to_string(),
                );
            }
        }
        // Each dimension cites WCAG references and source beads.
        assert!(dim["wcag"].as_array().is_some_and(|a| !a.is_empty()));
        assert!(
            dim["source_beads"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
    }
    assert!(!evidence.is_empty(), "matrix must cite evidence tests");

    // Every claimed evidence test must exist as a #[test] fn in this file, so
    // the matrix and the suite cannot drift.
    let suite_src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pane_a11y_compliance_a11y.rs"),
    )
    .expect("read suite source");
    for name in &evidence {
        let needle = format!("fn {name}(");
        assert!(
            suite_src.contains(&needle),
            "matrix cites evidence test `{name}` but no such test exists in the suite"
        );
    }

    log_evidence(
        "both",
        "compliance_matrix_is_consistent_with_suite",
        &["parse", "cross-check"],
        &format!("{} evidence tests", evidence.len()),
        true,
    );
}
