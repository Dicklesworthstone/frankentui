#![forbid(unsafe_code)]

//! Integration tests for the focus management system (bd-1n5t.5).
//!
//! These tests exercise the full focus stack: graph data structure,
//! manager coordination, spatial navigation, and focus trapping — all
//! working together as they would in a real widget hierarchy.
//!
//! # Invariants tested
//!
//! 1. Focus is always on a focusable node (or `None`).
//! 2. Trap confinement: when a trap is active, focus cannot escape the group.
//! 3. History integrity: `focus_back` never creates forward entries.
//! 4. Spatial determinism: same layout always produces same navigation path.
//! 5. Tab order respects `tab_index` sorting with ID tiebreak.
//! 6. Explicit edges always override spatial search.

use ftui_core::geometry::Rect;
use ftui_widgets::focus::{
    FocusEvent, FocusGraph, FocusManager, FocusNode, NavDirection, build_spatial_edges,
    spatial_navigate,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn node(id: u64, x: u16, y: u16, w: u16, h: u16, tab: i32) -> FocusNode {
    FocusNode::new(id, Rect::new(x, y, w, h)).with_tab_index(tab)
}

/// Build a 2x2 grid layout for integration tests.
///
/// ```text
///   [1]  [2]
///   [3]  [4]
/// ```
fn grid_2x2() -> FocusManager {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));
    fm.graph_mut().insert(node(2, 20, 0, 10, 3, 1));
    fm.graph_mut().insert(node(3, 0, 6, 10, 3, 2));
    fm.graph_mut().insert(node(4, 20, 6, 10, 3, 3));
    fm
}

/// Build a 3x3 grid layout.
///
/// ```text
///   [1]  [2]  [3]
///   [4]  [5]  [6]
///   [7]  [8]  [9]
/// ```
fn grid_3x3() -> FocusManager {
    let mut fm = FocusManager::new();
    for row in 0..3u16 {
        for col in 0..3u16 {
            let id = (row * 3 + col + 1) as u64;
            fm.graph_mut()
                .insert(node(id, col * 12, row * 4, 10, 3, id as i32));
        }
    }
    fm
}

/// Simulate a form-like layout: sidebar + main content with stacked fields.
///
/// ```text
///   [sidebar: 10]
///   [field_a: 20]  [field_b: 21]  [field_c: 22]
///   [submit: 30]   [cancel: 31]
/// ```
fn form_layout() -> FocusManager {
    let mut fm = FocusManager::new();
    // Sidebar (short, left column — center at y=1.5)
    fm.graph_mut().insert(node(10, 0, 0, 15, 3, 0));
    // Row of form fields (y=0, centers at y=1.5)
    fm.graph_mut().insert(node(20, 18, 0, 12, 3, 1));
    fm.graph_mut().insert(node(21, 32, 0, 12, 3, 2));
    fm.graph_mut().insert(node(22, 46, 0, 12, 3, 3));
    // Buttons (y=6)
    fm.graph_mut().insert(node(30, 18, 6, 10, 3, 4));
    fm.graph_mut().insert(node(31, 30, 6, 10, 3, 5));
    fm
}

// ===========================================================================
// Spatial + Manager integration
// ===========================================================================

#[test]
fn spatial_navigation_integrates_with_manager() {
    let mut fm = grid_2x2();
    build_spatial_edges(fm.graph_mut());

    assert!(fm.focus(1).is_none());
    assert!(fm.navigate(NavDirection::Right));
    assert_eq!(fm.current(), Some(2));
    assert!(fm.navigate(NavDirection::Down));
    assert_eq!(fm.current(), Some(4));
    assert!(fm.navigate(NavDirection::Left));
    assert_eq!(fm.current(), Some(3));
    assert!(fm.navigate(NavDirection::Up));
    assert_eq!(fm.current(), Some(1));
}

#[test]
fn explicit_edges_override_spatial_fallback() {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));
    fm.graph_mut().insert(node(2, 20, 0, 10, 3, 1));
    fm.graph_mut().insert(node(3, 40, 0, 10, 3, 2));

    build_spatial_edges(fm.graph_mut());
    // Override: right from 1 goes to 3, not spatially nearest 2.
    fm.graph_mut().connect(1, NavDirection::Right, 3);

    fm.focus(1);
    assert!(fm.navigate(NavDirection::Right));
    assert_eq!(fm.current(), Some(3));
}

#[test]
fn trap_confines_tab_order_to_group() {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));
    fm.graph_mut().insert(node(2, 20, 0, 10, 3, 1));
    fm.graph_mut().insert(node(3, 40, 0, 10, 3, 2));

    fm.create_group(10, vec![1, 2]);
    fm.push_trap(10);

    assert_eq!(fm.current(), Some(1));
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(2));
    // Group wraps by default, so next goes back to 1.
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(1));
}

// ===========================================================================
// Form-like widget hierarchy
// ===========================================================================

#[test]
fn form_tab_order_traverses_all_fields() {
    let mut fm = form_layout();
    fm.focus_first();
    assert_eq!(fm.current(), Some(10)); // sidebar

    let expected = [10, 20, 21, 22, 30, 31];
    for &exp in &expected[1..] {
        assert!(fm.focus_next(), "focus_next should succeed for id={exp}");
        assert_eq!(fm.current(), Some(exp));
    }
}

#[test]
fn form_spatial_navigation_sidebar_to_fields() {
    let mut fm = form_layout();
    fm.focus(10); // sidebar

    // Spatial right from sidebar should reach the first field row.
    assert!(fm.navigate(NavDirection::Right));
    assert_eq!(fm.current(), Some(20));
}

#[test]
fn form_spatial_navigation_fields_down_to_buttons() {
    let mut fm = form_layout();
    fm.focus(20); // first field

    // Down from first field should reach submit button.
    assert!(fm.navigate(NavDirection::Down));
    assert_eq!(fm.current(), Some(30));
}

#[test]
fn form_history_tracks_user_journey() {
    let mut fm = form_layout();

    fm.focus(10);
    fm.focus(20);
    fm.focus(21);
    fm.focus(22);

    // Back should retrace: 22 → 21 → 20 → 10.
    assert!(fm.focus_back());
    assert_eq!(fm.current(), Some(21));
    assert!(fm.focus_back());
    assert_eq!(fm.current(), Some(20));
    assert!(fm.focus_back());
    assert_eq!(fm.current(), Some(10));
}

// ===========================================================================
// Modal dialog (trap) integration
// ===========================================================================

#[test]
fn modal_trap_confines_spatial_navigation() {
    let mut fm = grid_3x3();

    // Create a group for the "modal" containing only nodes 5 and 6 (center row, right two).
    fm.create_group(1, vec![5, 6]);
    fm.focus(5);
    fm.push_trap(1);

    // Spatial: right from 5 → 6 (allowed, both in group).
    assert!(fm.navigate(NavDirection::Right));
    assert_eq!(fm.current(), Some(6));

    // Spatial: right from 6 → nothing in group.
    assert!(!fm.navigate(NavDirection::Right));
    assert_eq!(fm.current(), Some(6));

    // Spatial: down from 6 → 9 is outside group, blocked.
    assert!(!fm.navigate(NavDirection::Down));
    assert_eq!(fm.current(), Some(6));

    // Direct focus outside group also blocked.
    assert!(fm.focus(1).is_none());
    assert_eq!(fm.current(), Some(6));
}

#[test]
fn modal_pop_restores_previous_focus() {
    let mut fm = grid_3x3();

    fm.focus(1);
    fm.create_group(1, vec![5, 6]);
    fm.push_trap(1);

    // Trap auto-focused 5.
    assert_eq!(fm.current(), Some(5));

    // Pop trap restores focus to 1 (where we were before the trap).
    fm.pop_trap();
    assert_eq!(fm.current(), Some(1));
    assert!(!fm.is_trapped());
}

#[test]
fn nested_traps_restore_correctly() {
    let mut fm = FocusManager::new();
    for i in 1..=6 {
        fm.graph_mut()
            .insert(node(i, (i as u16 - 1) * 12, 0, 10, 3, i as i32));
    }

    fm.create_group(1, vec![1, 2]);
    fm.create_group(2, vec![3, 4]);

    fm.focus(5); // Start outside any group.

    // Push first trap.
    fm.push_trap(1);
    assert_eq!(fm.current(), Some(1));

    // Push nested trap.
    fm.push_trap(2);
    assert_eq!(fm.current(), Some(3));

    // Pop inner trap — back to group 1.
    fm.pop_trap();
    assert!(fm.is_trapped());
    assert_eq!(fm.current(), Some(1));

    // Pop outer trap — back to original.
    fm.pop_trap();
    assert!(!fm.is_trapped());
    assert_eq!(fm.current(), Some(5));
}

// ===========================================================================
// Focus events
// ===========================================================================

#[test]
fn events_emitted_on_focus_changes() {
    let mut fm = grid_2x2();

    fm.focus(1);
    assert_eq!(
        fm.take_focus_event(),
        Some(FocusEvent::FocusGained { id: 1 })
    );

    fm.focus(2);
    assert_eq!(
        fm.take_focus_event(),
        Some(FocusEvent::FocusMoved { from: 1, to: 2 })
    );

    fm.blur();
    assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 2 }));
}

#[test]
fn events_emitted_on_navigation() {
    let mut fm = grid_2x2();
    fm.focus(1);
    fm.take_focus_event(); // consume FocusGained

    fm.navigate(NavDirection::Right);
    assert_eq!(
        fm.take_focus_event(),
        Some(FocusEvent::FocusMoved { from: 1, to: 2 })
    );
}

#[test]
fn events_emitted_on_tab_navigation() {
    let mut fm = grid_2x2();
    fm.focus_first();
    fm.take_focus_event(); // consume

    fm.focus_next();
    assert_eq!(
        fm.take_focus_event(),
        Some(FocusEvent::FocusMoved { from: 1, to: 2 })
    );
}

#[test]
fn events_emitted_on_focus_back() {
    let mut fm = grid_2x2();
    fm.focus(1);
    fm.focus(2);
    fm.take_focus_event();

    fm.focus_back();
    assert_eq!(
        fm.take_focus_event(),
        Some(FocusEvent::FocusMoved { from: 2, to: 1 })
    );
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
fn empty_manager_operations() {
    let mut fm = FocusManager::new();

    assert_eq!(fm.current(), None);
    assert!(!fm.focus_next());
    assert!(!fm.focus_prev());
    assert!(!fm.focus_first());
    assert!(!fm.focus_last());
    assert!(!fm.focus_back());
    assert!(!fm.navigate(NavDirection::Right));
    assert!(fm.blur().is_none());
    assert!(fm.take_focus_event().is_none());
}

#[test]
fn single_node_tab_wraps_to_self() {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));

    fm.focus_first();
    assert_eq!(fm.current(), Some(1));

    // Next with only one node should not change (no other focusable node).
    assert!(!fm.focus_next());
    assert_eq!(fm.current(), Some(1));
}

#[test]
fn unfocusable_nodes_skipped_in_tab_order() {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));
    fm.graph_mut()
        .insert(node(2, 12, 0, 10, 3, 1).with_focusable(false));
    fm.graph_mut().insert(node(3, 24, 0, 10, 3, 2));

    fm.focus_first();
    assert_eq!(fm.current(), Some(1));

    fm.focus_next();
    assert_eq!(fm.current(), Some(3)); // skipped 2
}

#[test]
fn negative_tab_index_skipped() {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));
    fm.graph_mut().insert(node(2, 12, 0, 10, 3, -1));
    fm.graph_mut().insert(node(3, 24, 0, 10, 3, 1));

    fm.focus_first();
    assert_eq!(fm.current(), Some(1));

    fm.focus_next();
    assert_eq!(fm.current(), Some(3)); // skipped 2 (negative tab_index)
}

#[test]
fn focus_nonexistent_node_fails() {
    let mut fm = grid_2x2();
    assert!(fm.focus(999).is_none());
    assert_eq!(fm.current(), None);
}

#[test]
fn focus_unfocusable_node_fails() {
    let mut fm = FocusManager::new();
    fm.graph_mut()
        .insert(node(1, 0, 0, 10, 3, 0).with_focusable(false));
    assert!(fm.focus(1).is_none());
    assert_eq!(fm.current(), None);
}

#[test]
fn focus_same_node_returns_self() {
    let mut fm = grid_2x2();
    fm.focus(1);
    // Focusing same node returns Some(1) (the previous, which is also 1).
    let prev = fm.focus(1);
    assert_eq!(prev, Some(1));
    assert_eq!(fm.current(), Some(1));
}

#[test]
fn history_deduplication() {
    let mut fm = grid_2x2();
    fm.focus(1);
    fm.focus(2);
    fm.focus(2); // same node — should not push duplicate to history

    // History should be [1] not [1, 2].
    assert!(fm.focus_back());
    assert_eq!(fm.current(), Some(1));
    assert!(!fm.focus_back()); // no more history
}

#[test]
fn clear_history_empties_stack() {
    let mut fm = grid_2x2();
    fm.focus(1);
    fm.focus(2);
    fm.focus(3);

    fm.clear_history();
    assert!(!fm.focus_back());
}

#[test]
fn remove_node_while_focused() {
    let mut fm = grid_2x2();
    fm.focus(1);

    // Remove the focused node from the graph.
    let _ = fm.graph_mut().remove(1);

    // Focus is still set to 1 (manager doesn't auto-clear), but navigating
    // away should work since the manager will skip nodes that no longer exist.
    assert!(!fm.navigate(NavDirection::Right));
}

#[test]
fn add_to_group_and_remove_from_group() {
    let mut fm = FocusManager::new();
    fm.graph_mut().insert(node(1, 0, 0, 10, 3, 0));
    fm.graph_mut().insert(node(2, 12, 0, 10, 3, 1));
    fm.graph_mut().insert(node(3, 24, 0, 10, 3, 2));

    fm.create_group(1, vec![1, 2]);
    fm.add_to_group(1, 3);

    fm.push_trap(1);
    // All three should be accessible in the trap.
    fm.focus(1);
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(2));
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(3));

    // Remove 3 from group, then try to focus it.
    fm.pop_trap();
    fm.remove_from_group(1, 3);
    fm.push_trap(1);

    fm.focus(1);
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(2));
    // Group wraps by default, so next goes back to 1.
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(1));
}

// ===========================================================================
// Graph + Spatial standalone integration
// ===========================================================================

#[test]
fn spatial_navigate_ignores_unfocusable_nodes() {
    let mut g = FocusGraph::new();
    g.insert(node(1, 0, 0, 10, 3, 0));
    g.insert(node(2, 12, 0, 10, 3, 1).with_focusable(false));
    g.insert(node(3, 24, 0, 10, 3, 2));

    let target = spatial_navigate(&g, 1, NavDirection::Right);
    assert_eq!(target, Some(3));
}

#[test]
fn build_spatial_edges_does_not_overwrite_explicit() {
    let mut g = FocusGraph::new();
    g.insert(node(1, 0, 0, 10, 3, 0));
    g.insert(node(2, 12, 0, 10, 3, 1));
    g.insert(node(3, 24, 0, 10, 3, 2));

    // Explicit: right from 1 goes to 3 (not spatially nearest 2).
    g.connect(1, NavDirection::Right, 3);
    build_spatial_edges(&mut g);

    assert_eq!(g.navigate(1, NavDirection::Right), Some(3));
}

#[test]
fn cycle_detection_on_tab_chain() {
    let mut g = FocusGraph::new();
    for i in 1..=5 {
        g.insert(node(i, (i as u16 - 1) * 12, 0, 10, 3, i as i32));
    }
    g.build_tab_chain(true); // wrapping

    let cycle = g.find_cycle(1);
    assert!(cycle.is_some());
    let c = cycle.unwrap();
    assert_eq!(c.len(), 6); // 5 nodes + closing
    assert_eq!(c.first(), c.last());
}

#[test]
fn no_cycle_in_linear_chain() {
    let mut g = FocusGraph::new();
    for i in 1..=5 {
        g.insert(node(i, (i as u16 - 1) * 12, 0, 10, 3, i as i32));
    }
    g.build_tab_chain(false); // no wrapping

    assert!(g.find_cycle(1).is_none());
}

// ===========================================================================
// Property tests
// ===========================================================================

/// Invariant: focus is always `None` or a valid focusable node ID.
#[test]
fn property_focus_always_valid_or_none() {
    let mut fm = grid_3x3();

    // Exercise all navigation methods.
    fm.focus_first();
    for _ in 0..20 {
        fm.focus_next();
    }
    for _ in 0..20 {
        fm.focus_prev();
    }
    fm.focus_last();
    fm.navigate(NavDirection::Right);
    fm.navigate(NavDirection::Down);
    fm.navigate(NavDirection::Left);
    fm.navigate(NavDirection::Up);
    fm.focus_back();

    // After all that, current must be a valid focusable node or None.
    if let Some(id) = fm.current() {
        let node = fm.graph().get(id);
        assert!(node.is_some(), "focused node {id} must exist in graph");
        assert!(
            node.unwrap().is_focusable,
            "focused node {id} must be focusable"
        );
    }
}

/// Invariant: focus_back never grows the history stack.
#[test]
fn property_focus_back_does_not_grow_history() {
    let mut fm = grid_3x3();

    // Build some history.
    fm.focus(1);
    fm.focus(2);
    fm.focus(3);
    fm.focus(4);
    fm.focus(5);

    // Back should not push new entries — repeated back should converge to empty.
    let mut backs = 0;
    while fm.focus_back() {
        backs += 1;
        assert!(backs <= 10, "focus_back should converge, not loop");
    }
}

/// Invariant: spatial navigation is deterministic.
#[test]
fn property_spatial_deterministic() {
    let mut fm = grid_3x3();
    build_spatial_edges(fm.graph_mut());

    for _ in 0..50 {
        for id in 1..=9 {
            for dir in [
                NavDirection::Up,
                NavDirection::Down,
                NavDirection::Left,
                NavDirection::Right,
            ] {
                let a = spatial_navigate(fm.graph(), id, dir);
                let b = spatial_navigate(fm.graph(), id, dir);
                assert_eq!(a, b, "Non-deterministic: id={id}, dir={dir:?}");
            }
        }
    }
}

/// Invariant: trap prevents escape in all navigation modes.
#[test]
fn property_trap_confinement_comprehensive() {
    let mut fm = grid_3x3();
    fm.create_group(1, vec![1, 2, 4, 5]);
    fm.focus(1);
    fm.push_trap(1);

    // Try every direction from every node in the group.
    for &start in &[1, 2, 4, 5] {
        fm.focus(start);
        for dir in NavDirection::ALL {
            fm.navigate(dir);
            // After navigation, focus must still be in the group.
            if let Some(current) = fm.current() {
                assert!(
                    [1, 2, 4, 5].contains(&current),
                    "Focus escaped trap to {current} from {start} via {dir:?}"
                );
            }
        }
    }

    // Also try direct focus on nodes outside the group.
    for &outside in &[3, 6, 7, 8, 9] {
        assert!(
            fm.focus(outside).is_none(),
            "Should not be able to focus {outside} while trapped"
        );
    }
}

/// Invariant: focus_first and focus_last bracket the tab order.
#[test]
fn property_first_last_bracket_tab_order() {
    let mut fm = form_layout();
    let tab_order = fm.graph().tab_order();

    fm.focus_first();
    assert_eq!(fm.current(), Some(*tab_order.first().unwrap()));

    fm.focus_last();
    assert_eq!(fm.current(), Some(*tab_order.last().unwrap()));
}

/// Invariant: full tab cycle visits every focusable node exactly once.
#[test]
fn property_tab_cycle_visits_all() {
    let mut fm = form_layout();
    let tab_order = fm.graph().tab_order();
    let n = tab_order.len();

    fm.focus_first();
    let mut visited = vec![fm.current().unwrap()];

    for _ in 0..(n - 1) {
        assert!(fm.focus_next());
        visited.push(fm.current().unwrap());
    }

    // All unique.
    let mut deduped = visited.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(visited.len(), deduped.len(), "Tab cycle visited duplicates");
    assert_eq!(visited.len(), n, "Tab cycle missed nodes");
}

// ===========================================================================
// Performance gates
// ===========================================================================

#[test]
fn perf_build_spatial_edges_10x10() {
    let mut fm = FocusManager::new();
    for row in 0..10u16 {
        for col in 0..10u16 {
            let id = (row * 10 + col + 1) as u64;
            fm.graph_mut()
                .insert(node(id, col * 12, row * 4, 10, 3, id as i32));
        }
    }

    let start = std::time::Instant::now();
    build_spatial_edges(fm.graph_mut());
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_micros() < 50_000,
        "build_spatial_edges(100 nodes) took {}us (budget: 50000us)",
        elapsed.as_micros()
    );
}

#[test]
fn perf_full_navigation_sequence_100_nodes() {
    let mut fm = FocusManager::new();
    for row in 0..10u16 {
        for col in 0..10u16 {
            let id = (row * 10 + col + 1) as u64;
            fm.graph_mut()
                .insert(node(id, col * 12, row * 4, 10, 3, id as i32));
        }
    }
    build_spatial_edges(fm.graph_mut());

    fm.focus(1);

    let start = std::time::Instant::now();
    for _ in 0..1000 {
        fm.navigate(NavDirection::Right);
        fm.navigate(NavDirection::Down);
        fm.navigate(NavDirection::Left);
        fm.navigate(NavDirection::Up);
        fm.focus_next();
        fm.focus_prev();
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_micros() < 100_000,
        "6000 navigation ops took {}us (budget: 100000us)",
        elapsed.as_micros()
    );
}

#[test]
fn perf_focus_history_1000_entries() {
    let mut fm = FocusManager::new();
    for i in 1..=1000 {
        fm.graph_mut().insert(node(i, 0, 0, 10, 3, i as i32));
    }

    let start = std::time::Instant::now();
    for i in 1..=1000 {
        fm.focus(i);
    }
    // Unwind the entire history.
    while fm.focus_back() {}
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_micros() < 50_000,
        "1000 focus + unwind took {}us (budget: 50000us)",
        elapsed.as_micros()
    );
}
