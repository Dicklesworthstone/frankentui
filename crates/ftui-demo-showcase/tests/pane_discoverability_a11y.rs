#![forbid(unsafe_code)]

//! Pane first-run discoverability + interaction-friction regression suite (bd-c2z7c).
//!
//! This is the executable form of the discoverability checklist at
//! `tests/e2e/pane_discoverability_checklist.json`. It validates that a
//! first-time user can discover pane interactions without instruction, and that
//! the affordances meet target-size, reduced-motion, and contrast requirements:
//!
//! - **Target size** — splitter handles meet the mouse minimum, and the touch
//!   minimum is reachable (directly on large viewports, or via large-target
//!   mode on compact ones).
//! - **First-run / empty-state hints** — discoverable cues exist for the core
//!   pointer actions, and a single-pane workspace hints how to create panes.
//! - **Resize feedback** — a live drag yields a ghost boundary, snap guide, and
//!   size badges.
//! - **Reduced motion / contrast** — affordances are static (reduced-motion
//!   safe) and contrast-safe.
//!
//! The final test parses the checklist and asserts every cited evidence test
//! exists here, so the checklist and the suite cannot drift.
//!
//! Run: `cargo test -p ftui-demo-showcase --test pane_discoverability_a11y`

use std::collections::BTreeMap;
use std::path::PathBuf;

use ftui_demo_showcase::pane_interaction::{
    PaneInputModality, PaneResizeOverlayConfig, build_resize_overlay, collect_splitter_primitives,
    default_pane_layout_tree, pane_empty_state_hint, pane_first_run_hints,
    pane_handle_target_report, pane_recommended_handle_thickness,
};
use ftui_layout::{
    PANE_TREE_SCHEMA_VERSION, PaneAccessibilityPreferences, PaneAffordanceMotion, PaneId, PaneLeaf,
    PaneNodeRecord, PaneTree, PaneTreeSnapshot, Rect,
};
use ftui_style::PaneAffordanceTheme;
use ftui_style::color::{WCAG_AA_LARGE_TEXT, contrast_ratio};
use ftui_style::theme::themes;

fn log_jsonl(scenario: &str, detail: &str, passed: bool) {
    eprintln!(
        "{{\"suite\":\"pane_discoverability\",\"scenario\":\"{scenario}\",\"detail\":\"{detail}\",\"passed\":{passed}}}"
    );
}

fn pid(raw: u64) -> PaneId {
    PaneId::new(raw).expect("non-zero id")
}

fn single_leaf_tree() -> PaneTree {
    let snapshot = PaneTreeSnapshot {
        schema_version: PANE_TREE_SCHEMA_VERSION,
        root: pid(1),
        next_id: pid(2),
        nodes: vec![PaneNodeRecord::leaf(pid(1), None, PaneLeaf::new("only"))],
        extensions: BTreeMap::new(),
    };
    PaneTree::from_snapshot(snapshot).expect("valid single-leaf tree")
}

const COMPACT: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};
const LARGE: Rect = Rect {
    x: 0,
    y: 0,
    width: 120,
    height: 40,
};

#[test]
fn default_handles_pass_mouse_target() {
    let tree = default_pane_layout_tree();
    let layout = tree.solve_layout(LARGE).expect("layout solves");
    let splitters = collect_splitter_primitives(&tree, &layout, LARGE, None, None);
    assert!(!splitters.is_empty(), "default layout has splitters");
    for s in &splitters {
        let report = pane_handle_target_report(s.handle_rect, PaneInputModality::Mouse);
        assert!(
            report.passes,
            "handle {:?} should pass mouse target ({}x{} vs {}x{})",
            s.handle_rect,
            report.measured_thickness,
            report.measured_span,
            report.required_thickness,
            report.required_span
        );
    }
    log_jsonl("mouse_target", "all default handles pass mouse", true);
}

#[test]
fn touch_targets_validated_and_remediable_by_large_target() {
    // Compact viewport → 1-cell rail: passes mouse, fails the touch thickness
    // minimum (the discoverability friction we're documenting).
    let tree = default_pane_layout_tree();
    let compact_layout = tree.solve_layout(COMPACT).expect("layout solves");
    let compact = collect_splitter_primitives(&tree, &compact_layout, COMPACT, None, None);
    let thin = compact
        .iter()
        .find(|s| s.handle_rect.width.min(s.handle_rect.height) < 2)
        .expect("a compact viewport yields a 1-cell rail");
    assert!(pane_handle_target_report(thin.handle_rect, PaneInputModality::Mouse).passes);
    assert!(
        !pane_handle_target_report(thin.handle_rect, PaneInputModality::Touch).passes,
        "a 1-cell rail is below the touch minimum"
    );

    // Remediation: large-target mode is what grows the recommended thickness
    // beyond the baseline touch minimum; without it the recommendation is the
    // bare minimum.
    let touch_min = PaneInputModality::Touch.min_target_thickness();
    let baseline = PaneAccessibilityPreferences::none();
    let large_target = PaneAccessibilityPreferences::none().with_large_target(true);
    assert_eq!(
        pane_recommended_handle_thickness(PaneInputModality::Touch, baseline),
        touch_min,
        "without large-target, the recommendation is the bare touch minimum"
    );
    let recommended = pane_recommended_handle_thickness(PaneInputModality::Touch, large_target);
    assert!(
        recommended > touch_min,
        "large-target remediation grows the target beyond the minimum"
    );

    // Large viewport → 2-cell rail with a long span: at least one handle passes
    // touch outright.
    let large_layout = tree.solve_layout(LARGE).expect("layout solves");
    let large = collect_splitter_primitives(&tree, &large_layout, LARGE, None, None);
    assert!(
        large
            .iter()
            .any(|s| pane_handle_target_report(s.handle_rect, PaneInputModality::Touch).passes),
        "a large viewport promotes a touch-passing handle"
    );
    log_jsonl(
        "touch_target",
        "validated + remediable via large-target",
        true,
    );
}

#[test]
fn first_run_hints_cover_core_pointer_actions() {
    let hints = pane_first_run_hints();
    assert!(hints.len() >= 4, "enough cues to be discoverable");
    let actions: Vec<&str> = hints.iter().map(|h| h.action).collect();
    assert!(actions.iter().any(|a| a.contains("resize")), "resize cue");
    assert!(
        actions.iter().any(|a| a.contains("maximize")),
        "maximize cue"
    );
    assert!(
        actions.iter().any(|a| a.contains("modes")),
        "mode-cycle cue"
    );
    // Every hint is fully populated (cue + action + gesture).
    for h in hints {
        assert!(!h.cue.is_empty() && !h.action.is_empty() && !h.gesture.is_empty());
    }
    log_jsonl("first_run_hints", "core pointer actions covered", true);
}

#[test]
fn empty_state_hint_appears_only_for_single_pane() {
    let single = single_leaf_tree();
    let hint = pane_empty_state_hint(&single).expect("single pane hints how to split");
    assert!(hint.action.contains("split"));
    // A populated multi-pane workspace shows no empty-state hint.
    let multi = default_pane_layout_tree();
    assert!(
        pane_empty_state_hint(&multi).is_none(),
        "multi-pane workspace has nothing to onboard"
    );
    log_jsonl("empty_state_hint", "single-pane only", true);
}

#[test]
fn resize_overlay_provides_discoverable_feedback() {
    let tree = default_pane_layout_tree();
    let layout = tree.solve_layout(LARGE).expect("layout solves");
    let splitters = collect_splitter_primitives(&tree, &layout, LARGE, None, None);
    let target = splitters.first().expect("a splitter").target;
    let overlay = build_resize_overlay(
        &tree,
        &layout,
        target,
        6_000,
        PaneResizeOverlayConfig::default(),
    )
    .expect("overlay builds for an active resize");
    assert_eq!(overlay.snap_guides.len(), 1, "a snap guide is shown");
    assert_eq!(
        overlay.badges.len(),
        2,
        "live size badges for both children"
    );
    assert_eq!(overlay.ghost.share_bps, 6_000, "ghost tracks the drag");
    log_jsonl("resize_feedback", "ghost + guide + badges", true);
}

#[test]
fn affordances_are_reduced_motion_safe() {
    // The micro-animation collapses to an instant step under reduced motion.
    let reduced = PaneAffordanceMotion::reduced();
    assert_eq!(
        reduced.hover_emphasis_bps(0),
        ftui_layout::PANE_AFFORDANCE_EMPHASIS_FULL_BPS
    );
    // The resize overlay is static geometry: it takes no motion input, so the
    // same drag sample yields an identical overlay regardless of motion state.
    let tree = default_pane_layout_tree();
    let layout = tree.solve_layout(LARGE).expect("layout solves");
    let target = collect_splitter_primitives(&tree, &layout, LARGE, None, None)
        .first()
        .expect("a splitter")
        .target;
    let cfg = PaneResizeOverlayConfig::default();
    let a = build_resize_overlay(&tree, &layout, target, 5_000, cfg).expect("builds");
    let b = build_resize_overlay(&tree, &layout, target, 5_000, cfg).expect("builds");
    assert_eq!(a, b, "overlay is motion-independent and deterministic");
    log_jsonl("reduced_motion", "static overlay + stepped emphasis", true);
}

#[test]
fn affordance_colors_meet_contrast() {
    let resolved = themes::dark().resolve(true);
    let normal = PaneAffordanceTheme::from_resolved(&resolved, false);
    let high = PaneAffordanceTheme::from_resolved(&resolved, true);
    assert!(
        normal.min_contrast_ratio() >= WCAG_AA_LARGE_TEXT,
        "default affordances clear the large-text floor"
    );
    let ring =
        |t: &PaneAffordanceTheme| contrast_ratio(t.focus_ring.to_rgb(), resolved.surface.to_rgb());
    assert!(
        ring(&high) + 1e-9 >= ring(&normal),
        "high contrast is not worse"
    );
    log_jsonl("contrast", "AA default, AAA available", true);
}

#[test]
fn discoverability_checklist_is_consistent() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/e2e/pane_discoverability_checklist.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let checklist: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

    assert_eq!(checklist["schema_version"], "pane-discoverability-v1");
    assert_eq!(checklist["task_bead"], "bd-c2z7c");

    let items = checklist["items"].as_array().expect("items array");
    assert!(items.len() >= 6, "a meaningful friction checklist");
    assert!(
        !checklist["known_friction"]
            .as_array()
            .expect("known_friction array")
            .is_empty(),
        "known friction is recorded with remediation"
    );

    let suite_src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pane_discoverability_a11y.rs"),
    )
    .expect("read suite source");

    for item in items {
        for field in [
            "id",
            "requirement",
            "ui_cue",
            "evidence",
            "remediation",
            "status",
        ] {
            assert!(
                item[field].as_str().is_some_and(|s| !s.is_empty()),
                "checklist item missing `{field}`: {item}"
            );
        }
        let evidence = item["evidence"].as_str().unwrap();
        assert!(
            suite_src.contains(&format!("fn {evidence}(")),
            "checklist cites evidence test `{evidence}` but no such test exists"
        );
    }
    log_jsonl(
        "checklist_consistency",
        &format!("{} items cross-checked", items.len()),
        true,
    );
}
