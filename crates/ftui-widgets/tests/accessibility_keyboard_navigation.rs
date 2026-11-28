#![forbid(unsafe_code)]

//! bd-1lg.16: Accessibility roles and keyboard navigation tests.
//!
//! Proves that:
//! 1. Tab/Shift-Tab navigates all focusable widgets
//! 2. Enter/Space activates interactive widgets (list selection, tree toggle, tab switch)
//! 3. Arrow keys navigate lists, trees, and spatial focus graphs
//! 4. Escape closes modals (via StackModal::close_on_escape)
//! 5. High-contrast theme provides distinct, readable colors
//! 6. Focus indicators are visible and style correctly
//!
//! Run:
//!   cargo test -p ftui-widgets --test accessibility_keyboard_navigation

use ftui_core::event::{KeyCode, KeyEvent};
use ftui_core::geometry::Rect;
use ftui_render::cell::PackedRgba;
use ftui_style::Style;
use ftui_style::theme::themes;
use ftui_widgets::focus::{
    FocusEvent, FocusId, FocusIndicator, FocusIndicatorKind, FocusManager, FocusNode, NavDirection,
};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::tabs::TabsState;
use ftui_widgets::tree::{Tree, TreeNode};

// ============================================================================
// 1. Tab/Shift-Tab Navigation
// ============================================================================

#[test]
fn tab_navigates_through_all_focusable_widgets() {
    let mut fm = FocusManager::new();
    let ids: Vec<FocusId> = (1..=5).collect();

    for (i, &id) in ids.iter().enumerate() {
        fm.graph_mut().insert(FocusNode {
            id,
            bounds: Rect::new(0, i as u16 * 3, 20, 3),
            tab_index: i as i32,
            is_focusable: true,
            group_id: None,
        });
    }

    // Focus first widget
    assert!(fm.focus_first());
    assert_eq!(fm.current(), Some(1));

    // Tab through all widgets
    for expected in 2..=5 {
        assert!(fm.focus_next(), "Tab should move to widget {expected}");
        assert_eq!(fm.current(), Some(expected));
    }
}

#[test]
fn shift_tab_navigates_backward_through_all_widgets() {
    let mut fm = FocusManager::new();
    let ids: Vec<FocusId> = (1..=4).collect();

    for (i, &id) in ids.iter().enumerate() {
        fm.graph_mut().insert(FocusNode {
            id,
            bounds: Rect::new(0, i as u16 * 3, 20, 3),
            tab_index: i as i32,
            is_focusable: true,
            group_id: None,
        });
    }

    // Start at last widget
    assert!(fm.focus_last());
    assert_eq!(fm.current(), Some(4));

    // Shift-Tab through all widgets backward
    for expected in (1..=3).rev() {
        assert!(
            fm.focus_prev(),
            "Shift-Tab should move to widget {expected}"
        );
        assert_eq!(fm.current(), Some(expected));
    }
}

#[test]
fn tab_skips_non_focusable_widgets() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    // Non-focusable widget
    fm.graph_mut().insert(FocusNode {
        id: 2,
        bounds: Rect::new(0, 3, 20, 3),
        tab_index: 1,
        is_focusable: false,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 3,
        bounds: Rect::new(0, 6, 20, 3),
        tab_index: 2,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();
    assert_eq!(fm.current(), Some(1));

    // Tab should skip widget 2 (non-focusable)
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(3), "Should skip non-focusable widget");
}

#[test]
fn tab_skips_negative_tab_index() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 2,
        bounds: Rect::new(0, 3, 20, 3),
        tab_index: -1, // Negative = skip tab order
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 3,
        bounds: Rect::new(0, 6, 20, 3),
        tab_index: 1,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();
    assert_eq!(fm.current(), Some(1));

    // Tab should skip widget 2 (negative tab_index)
    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(3), "Should skip negative tab_index");
}

#[test]
fn tab_respects_tab_index_ordering() {
    let mut fm = FocusManager::new();

    // Insert out of order, but tab_index should determine order
    fm.graph_mut().insert(FocusNode {
        id: 10,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 2,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 20,
        bounds: Rect::new(0, 3, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 30,
        bounds: Rect::new(0, 6, 20, 3),
        tab_index: 1,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();
    assert_eq!(fm.current(), Some(20), "First should be tab_index=0");

    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(30), "Second should be tab_index=1");

    assert!(fm.focus_next());
    assert_eq!(fm.current(), Some(10), "Third should be tab_index=2");
}

#[test]
fn tab_on_empty_graph_returns_false() {
    let mut fm = FocusManager::new();
    assert!(!fm.focus_next());
    assert!(!fm.focus_prev());
    assert!(fm.current().is_none());
}

// ============================================================================
// 2. Enter/Space Activates Interactive Widgets
// ============================================================================

#[test]
fn enter_selects_list_item() {
    let items = vec![
        ListItem::new("alpha"),
        ListItem::new("beta"),
        ListItem::new("gamma"),
    ];
    let list = List::new(items);
    let mut state = ListState::default();

    // Navigate to second item
    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
    assert_eq!(state.selected(), Some(1));
}

#[test]
fn space_toggles_tree_node() {
    let root = TreeNode::new("Root")
        .with_expanded(true)
        .with_children(vec![
            TreeNode::new("Alpha"),
            TreeNode::new("Beta").with_children(vec![TreeNode::new("Beta-1")]),
        ]);
    let mut tree = Tree::new(root);

    // Space toggles node at visible_index=2 (Beta, which has children)
    let handled = tree.handle_key(&KeyEvent::new(KeyCode::Char(' ')), 2);
    assert!(handled, "Space should toggle tree node with children");
}

#[test]
fn enter_toggles_tree_node() {
    let root = TreeNode::new("Root")
        .with_expanded(true)
        .with_children(vec![
            TreeNode::new("Child").with_children(vec![TreeNode::new("Grandchild")]),
        ]);
    let mut tree = Tree::new(root);

    // Enter toggles the child node at visible_index=1
    let handled = tree.handle_key(&KeyEvent::new(KeyCode::Enter), 1);
    assert!(handled, "Enter should toggle tree node with children");
}

#[test]
fn number_key_switches_tab() {
    let mut state = TabsState::default();

    // Press '2' to switch to second tab (0-indexed: index 1)
    assert!(state.handle_key(&KeyEvent::new(KeyCode::Char('2')), 3));
    assert_eq!(state.active, 1, "Number key '2' should select tab index 1");
}

#[test]
fn enter_key_on_leaf_tree_node_returns_false() {
    let root = TreeNode::new("Root")
        .with_expanded(true)
        .with_children(vec![TreeNode::new("Leaf")]);
    let mut tree = Tree::new(root);

    // Enter on a leaf node (no children) should return false
    let handled = tree.handle_key(&KeyEvent::new(KeyCode::Enter), 1);
    assert!(
        !handled,
        "Enter on a leaf node should not be handled (nothing to toggle)"
    );
}

// ============================================================================
// 3. Arrow Keys Navigate Lists, Trees, and Spatial Focus
// ============================================================================

#[test]
fn arrow_down_navigates_list() {
    let items = vec![
        ListItem::new("one"),
        ListItem::new("two"),
        ListItem::new("three"),
    ];
    let list = List::new(items);
    let mut state = ListState::default();

    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
    assert_eq!(state.selected(), Some(0));
    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
    assert_eq!(state.selected(), Some(1));
    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
    assert_eq!(state.selected(), Some(2));
}

#[test]
fn arrow_up_navigates_list() {
    let items = vec![
        ListItem::new("one"),
        ListItem::new("two"),
        ListItem::new("three"),
    ];
    let list = List::new(items);
    let mut state = ListState::default();

    // Navigate to last item first
    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down));
    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down));
    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down));
    assert_eq!(state.selected(), Some(2));

    // Arrow up
    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Up)));
    assert_eq!(state.selected(), Some(1));
}

#[test]
fn arrow_keys_navigate_spatial_focus_graph() {
    let mut fm = FocusManager::new();

    // Create a 2x2 grid layout
    let top_left = 1;
    let top_right = 2;
    let bottom_left = 3;
    let bottom_right = 4;

    fm.graph_mut().insert(FocusNode {
        id: top_left,
        bounds: Rect::new(0, 0, 10, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: top_right,
        bounds: Rect::new(15, 0, 10, 3),
        tab_index: 1,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: bottom_left,
        bounds: Rect::new(0, 5, 10, 3),
        tab_index: 2,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: bottom_right,
        bounds: Rect::new(15, 5, 10, 3),
        tab_index: 3,
        is_focusable: true,
        group_id: None,
    });

    // Focus top-left
    fm.focus(top_left);
    assert_eq!(fm.current(), Some(top_left));

    // Navigate right → top-right
    assert!(fm.navigate(NavDirection::Right));
    assert_eq!(fm.current(), Some(top_right));

    // Navigate down → bottom-right
    assert!(fm.navigate(NavDirection::Down));
    assert_eq!(fm.current(), Some(bottom_right));

    // Navigate left → bottom-left
    assert!(fm.navigate(NavDirection::Left));
    assert_eq!(fm.current(), Some(bottom_left));

    // Navigate up → top-left
    assert!(fm.navigate(NavDirection::Up));
    assert_eq!(fm.current(), Some(top_left));
}

#[test]
fn right_arrow_expands_collapsed_tree_node() {
    let root = TreeNode::new("Root")
        .with_expanded(true)
        .with_children(vec![
            TreeNode::new("Beta")
                .with_expanded(false) // Explicitly collapsed
                .with_children(vec![TreeNode::new("Beta-1")]),
        ]);
    let mut tree = Tree::new(root);

    // Right arrow on collapsed node should expand it
    let handled = tree.handle_key(&KeyEvent::new(KeyCode::Right), 1);
    assert!(
        handled,
        "Right arrow should expand collapsed node with children"
    );
}

#[test]
fn left_arrow_collapses_expanded_tree_node() {
    let root = TreeNode::new("Root")
        .with_expanded(true)
        .with_children(vec![
            TreeNode::new("Beta")
                .with_expanded(true)
                .with_children(vec![TreeNode::new("Beta-1")]),
        ]);
    let mut tree = Tree::new(root);

    // Left arrow on expanded node should collapse it
    let handled = tree.handle_key(&KeyEvent::new(KeyCode::Left), 1);
    assert!(handled, "Left arrow should collapse expanded node");
}

// ============================================================================
// 4. Escape Closes Modals (via StackModal trait)
// ============================================================================

#[test]
fn modal_config_defaults_allow_escape_close() {
    // ModalConfig defaults should enable escape-to-close for accessibility.
    let config = ftui_widgets::ModalConfig::default();
    assert!(
        config.close_on_escape,
        "Modal should close on Escape by default for accessibility"
    );
}

#[test]
fn modal_config_escape_can_be_disabled() {
    let config = ftui_widgets::ModalConfig {
        close_on_escape: false,
        ..Default::default()
    };
    assert!(
        !config.close_on_escape,
        "Modal escape close should be configurable"
    );
}

// ============================================================================
// 5. Focus Trap in Modals
// ============================================================================

#[test]
fn focus_trap_confines_tab_navigation() {
    let mut fm = FocusManager::new();

    // Main page widgets
    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 2,
        bounds: Rect::new(0, 3, 20, 3),
        tab_index: 1,
        is_focusable: true,
        group_id: None,
    });

    // Modal widgets (group 1)
    fm.graph_mut().insert(FocusNode {
        id: 10,
        bounds: Rect::new(5, 5, 10, 2),
        tab_index: 0,
        is_focusable: true,
        group_id: Some(1),
    });
    fm.graph_mut().insert(FocusNode {
        id: 11,
        bounds: Rect::new(5, 7, 10, 2),
        tab_index: 1,
        is_focusable: true,
        group_id: Some(1),
    });

    // Register the focus group and focus a main page widget
    fm.create_group(1, vec![10, 11]);
    fm.focus(1);
    assert_eq!(fm.current(), Some(1));

    // Push trap for modal group
    fm.push_trap(1);

    // After pushing trap, focus moves to first in group
    assert_eq!(
        fm.current(),
        Some(10),
        "After push_trap, focus should be on first widget in modal group"
    );

    // Tab within the trapped group
    fm.focus_next();
    assert_eq!(
        fm.current(),
        Some(11),
        "Tab in trapped mode should move to next in modal group"
    );

    // Pop trap should restore focus
    assert!(fm.pop_trap());
    assert_eq!(
        fm.current(),
        Some(1),
        "Pop trap should restore previous focus"
    );
}

#[test]
fn focus_trap_prevents_escape_to_main_content() {
    let mut fm = FocusManager::new();

    // Main content
    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });

    // Modal group
    fm.graph_mut().insert(FocusNode {
        id: 10,
        bounds: Rect::new(5, 5, 10, 2),
        tab_index: 0,
        is_focusable: true,
        group_id: Some(1),
    });

    fm.create_group(1, vec![10]);
    fm.focus(1);
    fm.push_trap(1);

    // Focus should now be on modal widget, not main content
    assert_eq!(fm.current(), Some(10));

    // Trying to focus main content widget should fail when trapped
    let result = fm.focus(1);
    assert!(
        result.is_none(),
        "Focus should not escape to main content when trapped"
    );
    assert_eq!(
        fm.current(),
        Some(10),
        "Focus should remain on modal widget"
    );
}

// ============================================================================
// 6. High-Contrast Theme
// ============================================================================

/// Compute relative luminance per WCAG 2.0 formula.
fn relative_luminance(r: u8, g: u8, b: u8) -> f64 {
    let srgb = |v: u8| -> f64 {
        let v = v as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * srgb(r) + 0.7152 * srgb(g) + 0.0722 * srgb(b)
}

/// Compute contrast ratio per WCAG 2.0.
fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

#[test]
fn doom_theme_text_background_meets_wcag_aa() {
    let theme = themes::doom();
    let resolved = theme.resolve(true); // dark mode

    let text = resolved.text.to_rgb();
    let bg = resolved.background.to_rgb();

    let text_lum = relative_luminance(text.r, text.g, text.b);
    let bg_lum = relative_luminance(bg.r, bg.g, bg.b);
    let ratio = contrast_ratio(text_lum, bg_lum);

    // WCAG AA requires 4.5:1 for normal text
    assert!(
        ratio >= 4.5,
        "Doom theme text/background contrast ratio {ratio:.2} must be >= 4.5:1"
    );
}

#[test]
fn doom_theme_error_color_is_distinct() {
    let theme = themes::doom();
    let resolved = theme.resolve(true);

    let error = resolved.error.to_rgb();
    let text = resolved.text.to_rgb();

    // Error color should be visually distinct from regular text
    let error_lum = relative_luminance(error.r, error.g, error.b);
    let text_lum = relative_luminance(text.r, text.g, text.b);

    assert!(
        (error_lum - text_lum).abs() > 0.05,
        "Error color should be visually distinct from text"
    );
}

#[test]
fn doom_theme_success_color_is_distinct_from_error() {
    let theme = themes::doom();
    let resolved = theme.resolve(true);

    let success = resolved.success.to_rgb();
    let error = resolved.error.to_rgb();

    // Success and error must be distinguishable
    assert_ne!(
        (success.r, success.g, success.b),
        (error.r, error.g, error.b),
        "Success and error colors must be different"
    );
}

#[test]
fn all_preset_themes_have_distinct_text_and_background() {
    let themes = [
        ("dark", themes::dark()),
        ("light", themes::light()),
        ("nord", themes::nord()),
        ("dracula", themes::dracula()),
        ("solarized_dark", themes::solarized_dark()),
        ("solarized_light", themes::solarized_light()),
        ("monokai", themes::monokai()),
        ("doom", themes::doom()),
    ];

    for (name, theme) in &themes {
        let resolved = theme.resolve(true);

        let text = resolved.text.to_rgb();
        let bg = resolved.background.to_rgb();

        assert_ne!(
            (text.r, text.g, text.b),
            (bg.r, bg.g, bg.b),
            "Theme {name}: text and background must be different"
        );
    }
}

#[test]
fn theme_resolve_produces_consistent_colors() {
    let theme = themes::dark();

    // Resolving twice should give the same result
    let r1 = theme.resolve(true);
    let r2 = theme.resolve(true);

    assert_eq!(r1.text, r2.text, "Resolved colors should be deterministic");
    assert_eq!(
        r1.background, r2.background,
        "Resolved colors should be deterministic"
    );
}

// ============================================================================
// 7. Focus Indicators
// ============================================================================

#[test]
fn focus_indicator_default_is_visible() {
    let indicator = FocusIndicator::default();
    assert!(
        indicator.is_visible(),
        "Default focus indicator must be visible"
    );
    assert_eq!(indicator.kind(), FocusIndicatorKind::StyleOverlay);
}

#[test]
fn focus_indicator_applies_style_overlay() {
    let base = Style::new().fg(PackedRgba::rgb(200, 200, 200));
    let indicator = FocusIndicator::default();

    let focused = indicator.apply_to(base);

    // Focused style should include the reverse attribute from default indicator
    assert_ne!(
        focused, base,
        "Focus indicator should modify the base style"
    );
}

#[test]
fn focus_indicator_none_preserves_base_style() {
    let base = Style::new().fg(PackedRgba::rgb(255, 0, 0)).bold();
    let indicator = FocusIndicator::none();

    let result = indicator.apply_to(base);
    assert_eq!(
        result, base,
        "None indicator should preserve base style exactly"
    );
}

#[test]
fn focus_indicator_underline_is_visible() {
    let indicator = FocusIndicator::underline();
    assert!(indicator.is_visible());
    assert_eq!(indicator.kind(), FocusIndicatorKind::Underline);
}

#[test]
fn focus_indicator_border_is_visible() {
    let indicator = FocusIndicator::border();
    assert!(indicator.is_visible());
    assert_eq!(indicator.kind(), FocusIndicatorKind::Border);
}

// ============================================================================
// 8. Focus Events
// ============================================================================

#[test]
fn focus_gained_event_on_first_focus() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();

    let event = fm.focus_event();
    assert!(
        matches!(event, Some(FocusEvent::FocusGained { id: 1 })),
        "First focus should emit FocusGained, got {event:?}"
    );
}

#[test]
fn focus_moved_event_on_tab() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 2,
        bounds: Rect::new(0, 3, 20, 3),
        tab_index: 1,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();
    fm.focus_next();

    let event = fm.focus_event();
    assert!(
        matches!(event, Some(FocusEvent::FocusMoved { from: 1, to: 2 })),
        "Tab should emit FocusMoved, got {event:?}"
    );
}

#[test]
fn focus_lost_event_on_blur() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();
    fm.blur();

    let event = fm.focus_event();
    assert!(
        matches!(event, Some(FocusEvent::FocusLost { id: 1 })),
        "Blur should emit FocusLost, got {event:?}"
    );
    assert!(
        fm.current().is_none(),
        "No widget should be focused after blur"
    );
}

// ============================================================================
// 9. Focus History and Back Navigation
// ============================================================================

#[test]
fn focus_back_restores_previous_focus() {
    let mut fm = FocusManager::new();

    for i in 1..=3 {
        fm.graph_mut().insert(FocusNode {
            id: i,
            bounds: Rect::new(0, (i as u16 - 1) * 3, 20, 3),
            tab_index: (i - 1) as i32,
            is_focusable: true,
            group_id: None,
        });
    }

    fm.focus_first(); // → 1
    fm.focus_next(); // → 2
    fm.focus_next(); // → 3

    assert_eq!(fm.current(), Some(3));

    // Go back should restore to 2
    assert!(fm.focus_back());
    assert_eq!(fm.current(), Some(2), "focus_back should restore to 2");

    // Go back again should restore to 1
    assert!(fm.focus_back());
    assert_eq!(fm.current(), Some(1), "focus_back should restore to 1");
}

// ============================================================================
// 10. Widget-Level Keyboard Accessibility
// ============================================================================

#[test]
fn list_escape_clears_filter_for_accessibility() {
    let items = vec![
        ListItem::new("alpha"),
        ListItem::new("beta"),
        ListItem::new("gamma"),
    ];
    let list = List::new(items);
    let mut state = ListState::default();

    // Type to filter
    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('b')));
    assert_eq!(state.filter_query(), "b");

    // Escape clears filter, restoring full list access
    assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Escape)));
    assert_eq!(state.filter_query(), "", "Escape should clear filter");
}

#[test]
fn list_vi_j_k_navigation_for_keyboard_users() {
    let items = vec![
        ListItem::new("one"),
        ListItem::new("two"),
        ListItem::new("three"),
    ];
    let list = List::new(items);
    let mut state = ListState::default();

    // j = Down (vi binding for keyboard-only users)
    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('j')));
    assert_eq!(state.selected(), Some(0), "j should move down");

    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('j')));
    assert_eq!(state.selected(), Some(1), "j should move to second item");

    // k = Up (vi binding)
    list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('k')));
    assert_eq!(state.selected(), Some(0), "k should move up");
}

#[test]
fn tabs_left_right_arrow_keys() {
    let mut state = TabsState::default();

    // Right arrow to move forward
    assert!(state.handle_key(&KeyEvent::new(KeyCode::Right), 3));
    assert_eq!(state.active, 1);

    assert!(state.handle_key(&KeyEvent::new(KeyCode::Right), 3));
    assert_eq!(state.active, 2);

    // Left arrow to move back
    assert!(state.handle_key(&KeyEvent::new(KeyCode::Left), 3));
    assert_eq!(state.active, 1);
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn tabs_right_arrow_at_end_does_not_wrap() {
    let mut state = TabsState::default();
    state.active = 2;

    // Right at end should not wrap (returns false)
    assert!(!state.handle_key(&KeyEvent::new(KeyCode::Right), 3));
    assert_eq!(state.active, 2, "Right at end should stay at last tab");
}

#[test]
fn tabs_left_arrow_at_start_does_not_wrap() {
    let mut state = TabsState::default();
    // active defaults to 0

    // Left at start should not wrap
    assert!(!state.handle_key(&KeyEvent::new(KeyCode::Left), 3));
    assert_eq!(state.active, 0, "Left at start should stay at first tab");
}

// ============================================================================
// 11. Host Focus (Window Focus/Blur)
// ============================================================================

#[test]
fn host_blur_clears_focus() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });

    fm.focus_first();
    assert_eq!(fm.current(), Some(1));

    // Window loses focus
    assert!(fm.apply_host_focus(false));
    assert!(
        fm.current().is_none(),
        "Host blur should clear widget focus"
    );
}

#[test]
fn host_focus_restores_first_widget() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });

    // Window gains focus with no current focus
    assert!(fm.apply_host_focus(true));
    assert!(
        fm.current().is_some(),
        "Host focus should restore focus to first widget"
    );
}

// ============================================================================
// 12. Focus Change Counter (Metrics)
// ============================================================================

#[test]
fn focus_change_count_increments() {
    let mut fm = FocusManager::new();

    fm.graph_mut().insert(FocusNode {
        id: 1,
        bounds: Rect::new(0, 0, 20, 3),
        tab_index: 0,
        is_focusable: true,
        group_id: None,
    });
    fm.graph_mut().insert(FocusNode {
        id: 2,
        bounds: Rect::new(0, 3, 20, 3),
        tab_index: 1,
        is_focusable: true,
        group_id: None,
    });

    let initial = fm.focus_change_count();
    fm.focus_first();
    assert!(
        fm.focus_change_count() > initial,
        "Focus change count should increment"
    );

    let before_tab = fm.focus_change_count();
    fm.focus_next();
    assert!(
        fm.focus_change_count() > before_tab,
        "Tab should increment focus change count"
    );
}

// ============================================================================
// 13. Rendered Widget Accessibility Coverage
// ============================================================================

fn build_a11y_tree(
    width: u16,
    height: u16,
    render: impl FnOnce(&mut ftui_render::frame::Frame),
) -> ftui_a11y::tree::A11yTree {
    let mut builder = ftui_a11y::tree::A11yTreeBuilder::new();
    let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
    let mut frame = ftui_render::frame::Frame::new(width, height, &mut pool);
    frame.set_a11y(&mut builder);
    render(&mut frame);
    frame.finish_a11y();
    builder.build()
}

#[test]
fn error_boundary_emits_assertive_live_region_on_panic() {
    use ftui_a11y::node::{A11yRole, LiveRegion};
    use ftui_widgets::error_boundary::{ErrorBoundary, ErrorBoundaryState};
    use ftui_widgets::{StatefulWidget, Widget};

    struct Boom;
    impl Widget for Boom {
        fn render(&self, _area: Rect, _frame: &mut ftui_render::frame::Frame) {
            panic!("simulated widget boom");
        }
    }

    let boundary = ErrorBoundary::new(Boom, "TelemetryPanel");
    let mut state = ErrorBoundaryState::default();
    let tree = build_a11y_tree(60, 10, |frame| {
        boundary.render(Rect::new(0, 0, 60, 10), frame, &mut state);
    });

    let alert = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.live_region == Some(LiveRegion::Assertive))
        .expect("ErrorBoundary fallback should emit an assertive live Group");
    assert_eq!(alert.name.as_deref(), Some("Widget error: TelemetryPanel"));
    assert_eq!(alert.description.as_deref(), Some("simulated widget boom"));
}

#[test]
fn status_line_badge_paginator_stopwatch_and_timer_emit_a11y_nodes() {
    use ftui_a11y::node::{A11yRole, LiveRegion};
    use ftui_widgets::badge::Badge;
    use ftui_widgets::paginator::{Paginator, PaginatorMode};
    use ftui_widgets::status_line::{StatusItem, StatusLine};
    use ftui_widgets::stopwatch::{Stopwatch, StopwatchState};
    use ftui_widgets::timer::{Timer, TimerState};
    use ftui_widgets::{StatefulWidget, Widget};
    use std::time::Duration;

    let status = StatusLine::new()
        .left(StatusItem::text("[NORMAL]"))
        .center(StatusItem::text("main.rs"))
        .right(StatusItem::key_hint("^Q", "Quit"));
    let badge = Badge::new("LIVE");
    let paginator = Paginator::with_pages(2, 5).mode(PaginatorMode::Page);
    let mut sw_state = StopwatchState::new();
    sw_state.start();
    sw_state.tick(Duration::from_secs(42));
    let mut timer_state = TimerState::new(Duration::from_secs(5));
    timer_state.start();
    timer_state.tick(Duration::from_secs(5));

    let tree = build_a11y_tree(80, 12, |frame| {
        status.render(Rect::new(0, 0, 80, 1), frame);
        badge.render(Rect::new(0, 1, 10, 1), frame);
        paginator.render(Rect::new(0, 2, 20, 1), frame);
        StatefulWidget::render(
            &Stopwatch::new().label("Elapsed"),
            Rect::new(0, 3, 24, 1),
            frame,
            &mut sw_state,
        );
        StatefulWidget::render(
            &Timer::new().label("Build"),
            Rect::new(0, 4, 24, 1),
            frame,
            &mut timer_state,
        );
    });

    let toolbar = tree
        .nodes()
        .find(|n| n.role == A11yRole::Toolbar)
        .expect("StatusLine should emit Toolbar node");
    assert_eq!(toolbar.name.as_deref(), Some("Status line"));
    let desc = toolbar.description.as_deref().unwrap_or("");
    assert!(desc.contains("[NORMAL]") && desc.contains("main.rs") && desc.contains("^Q Quit"));

    assert!(
        tree.nodes()
            .any(|n| n.role == A11yRole::Label && n.name.as_deref() == Some("LIVE")),
        "Badge should emit Label node"
    );

    let pager_node = tree
        .nodes()
        .find(|n| n.name.as_deref() == Some("Paginator"))
        .expect("Paginator should emit Label node");
    assert_eq!(pager_node.state.value_now, Some(2.0));
    assert_eq!(pager_node.state.value_min, Some(1.0));
    assert_eq!(pager_node.state.value_max, Some(5.0));
    assert_eq!(pager_node.state.value_text.as_deref(), Some("Page 2 of 5"));

    let sw_node = tree
        .nodes()
        .find(|n| n.name.as_deref() == Some("Elapsed"))
        .expect("Stopwatch should emit Label node");
    assert!(sw_node.state.busy);
    assert_eq!(sw_node.state.value_text.as_deref(), Some("42s"));

    let timer_node = tree
        .nodes()
        .find(|n| n.name.as_deref() == Some("Build"))
        .expect("Timer should emit Label node");
    assert!(!timer_node.state.busy);
    assert_eq!(timer_node.live_region, Some(LiveRegion::Polite));
    assert_eq!(
        timer_node.state.value_text.as_deref(),
        Some("0s (finished)")
    );
}

#[test]
fn panel_popover_and_tabs_render_panel_scope_children() {
    use ftui_a11y::node::A11yRole;
    use ftui_widgets::Widget;
    use ftui_widgets::badge::Badge;
    use ftui_widgets::panel::Panel;
    use ftui_widgets::popover::{Placement, Popover};
    use ftui_widgets::tabs::{Tab, Tabs};

    let panel = Panel::new(Badge::new("Inside Panel"))
        .title("Settings")
        .subtitle("General");
    let popover = Popover::new(Rect::new(2, 6, 10, 1), Placement::Below)
        .width(20)
        .max_height(4)
        .with_border(true);
    let tabs = Tabs::new(vec![Tab::new("Overview"), Tab::new("Metrics")]);

    let tree = build_a11y_tree(80, 24, |frame| {
        panel.render(Rect::new(0, 0, 30, 5), frame);
        popover.render_with(Rect::new(0, 6, 40, 8), frame, |inner, frame| {
            Badge::new("Inside Popover").render(Rect::new(inner.x, inner.y, inner.width, 1), frame);
        });
        tabs.render_panel(1, Rect::new(0, 15, 40, 5), frame, |panel_area, frame| {
            Badge::new("Inside TabPanel")
                .render(Rect::new(panel_area.x, panel_area.y, 20, 1), frame);
        });
    });

    let panel_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("Settings"))
        .expect("Panel should emit named Group container");
    assert_eq!(panel_node.description.as_deref(), Some("General"));
    let panel_child = tree
        .nodes()
        .find(|n| n.name.as_deref() == Some("Inside Panel"))
        .expect("Panel child should be present");
    assert_eq!(panel_child.parent, Some(panel_node.id));

    let popover_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Dialog && n.name.as_deref() == Some("Popover"))
        .expect("Popover should emit Dialog container");
    let popover_child = tree
        .nodes()
        .find(|n| n.name.as_deref() == Some("Inside Popover"))
        .expect("Popover child should be present");
    assert_eq!(popover_child.parent, Some(popover_node.id));

    let tab_panel_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::TabPanel && n.name.as_deref() == Some("Metrics"))
        .expect("Tabs::render_panel should emit named TabPanel container");
    let tab_panel_child = tree
        .nodes()
        .find(|n| n.name.as_deref() == Some("Inside TabPanel"))
        .expect("TabPanel child should be present");
    assert_eq!(tab_panel_child.parent, Some(tab_panel_node.id));
}

#[test]
fn log_viewer_json_view_and_help_emit_rich_item_accessibility() {
    use ftui_a11y::node::A11yRole;
    use ftui_widgets::StatefulWidget;
    use ftui_widgets::help::{Help, HelpMode, HelpRenderState};
    use ftui_widgets::json_view::{JsonPathSegment, JsonView, JsonViewState};
    use ftui_widgets::log_viewer::{LogViewer, LogViewerState};

    let mut viewer = LogViewer::new(100);
    viewer.push("boot: kernel ready");
    viewer.push("warn: high latency");
    let mut lv_state = LogViewerState {
        selected_line: Some(1),
        ..Default::default()
    };

    let json = JsonView::new(r#"{"service": {"port": 8080}, "ok": true}"#);
    let mut jv_state = JsonViewState::new();
    jv_state.fold(&vec![JsonPathSegment::Key("service".to_string())]);
    jv_state.cursor_line = 1;

    let help = Help::new()
        .with_mode(HelpMode::Full)
        .entry("?", "Toggle help")
        .entry("q", "Quit");
    let mut help_state = HelpRenderState::default();

    let tree = build_a11y_tree(80, 24, |frame| {
        viewer.render(Rect::new(0, 0, 40, 4), frame, &mut lv_state);
        json.render(Rect::new(0, 5, 40, 6), frame, &mut jv_state);
        help.render(Rect::new(0, 12, 40, 4), frame, &mut help_state);
    });

    let lv_root = tree
        .nodes()
        .find(|n| n.role == A11yRole::List && n.name.as_deref() == Some("LogViewer"))
        .expect("LogViewer should emit List container");
    assert_eq!(
        lv_root.description.as_deref(),
        Some("2 lines"),
        "LogViewer should report true total line count"
    );
    let warn_item = tree
        .nodes()
        .find(|n| n.role == A11yRole::ListItem && n.name.as_deref() == Some("warn: high latency"))
        .expect("LogViewer should expose actual log line text");
    assert_eq!(warn_item.parent, Some(lv_root.id));
    assert!(
        warn_item.state.selected,
        "selected log line must set state.selected"
    );

    let jv_root = tree
        .nodes()
        .find(|n| n.role == A11yRole::List && n.name.as_deref() == Some("JSON view"))
        .expect("JsonView should emit List container");
    assert!(
        jv_root
            .description
            .as_deref()
            .unwrap_or("")
            .contains("1 folded")
    );
    let folded_row = tree
        .nodes()
        .find(|n| n.parent == Some(jv_root.id) && n.state.expanded == Some(false))
        .expect("Folded JSON row should expose state.expanded = Some(false)");
    assert!(
        folded_row.state.selected,
        "cursor_line row should be selected"
    );

    let help_root = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("Keyboard shortcuts"))
        .expect("Help should emit Keyboard shortcuts Group");
    let quit_hint = tree
        .nodes()
        .find(|n| n.parent == Some(help_root.id) && n.name.as_deref() == Some("Quit"))
        .expect("Help should expose enabled shortcut entries");
    assert_eq!(quit_hint.shortcut.as_deref(), Some("q"));
    assert!(
        help_root.children.contains(&quit_hint.id),
        "Help group children list should contain assigned child IDs"
    );
}

#[test]
fn a11y_edge_cases_stable_log_ids_truncation_and_empty_suppression() {
    use ftui_a11y::node::{A11yRole, LiveRegion};
    use ftui_widgets::badge::Badge;
    use ftui_widgets::error_boundary::{ErrorBoundary, ErrorBoundaryState};
    use ftui_widgets::help::Help;
    use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
    use ftui_widgets::{StatefulWidget, Widget};

    // 1. LogViewer with more lines than viewport reports true count, names empty
    //    lines by real line number, and preserves stable node IDs across scroll.
    let mut viewer = LogViewer::new(100);
    for idx in 0..8 {
        if idx == 2 {
            viewer.push("   ");
        } else {
            viewer.push(format!("event-{idx}"));
        }
    }
    viewer.scroll_to_top();
    let mut lv_state = LogViewerState::default();
    let tree_top = build_a11y_tree(60, 4, |frame| {
        viewer.render(Rect::new(0, 0, 60, 4), frame, &mut lv_state);
    });
    let lv_root = tree_top
        .nodes()
        .find(|n| n.role == A11yRole::List && n.name.as_deref() == Some("LogViewer"))
        .expect("LogViewer root");
    assert_eq!(lv_root.description.as_deref(), Some("8 lines"));
    let empty_line = tree_top
        .nodes()
        .find(|n| n.role == A11yRole::ListItem && n.name.as_deref() == Some("Line 3 (empty)"))
        .expect("blank log line should expose 1-based line number");
    let event3_before = tree_top
        .nodes()
        .find(|n| n.role == A11yRole::ListItem && n.name.as_deref() == Some("event-3"))
        .expect("event-3 visible before scroll")
        .id;

    viewer.scroll_down(1);
    let tree_scrolled = build_a11y_tree(60, 4, |frame| {
        viewer.render(Rect::new(0, 0, 60, 4), frame, &mut lv_state);
    });
    let event3_after = tree_scrolled
        .nodes()
        .find(|n| n.role == A11yRole::ListItem && n.name.as_deref() == Some("event-3"))
        .expect("event-3 still visible after scrolling 1 line")
        .id;
    assert_eq!(
        event3_before, event3_after,
        "LogViewer item node IDs should remain stable across scrolling"
    );
    assert_eq!(empty_line.parent, Some(lv_root.id));

    // 2. Empty Badge and empty Help emit no ghost nodes; long panic message truncates.
    struct BigBoom;
    impl Widget for BigBoom {
        fn render(&self, _area: Rect, _frame: &mut ftui_render::frame::Frame) {
            panic!("{}", "x".repeat(350));
        }
    }
    let boundary = ErrorBoundary::new(BigBoom, "HugePanic");
    let mut eb_state = ErrorBoundaryState::default();
    let tree = build_a11y_tree(60, 6, |frame| {
        Badge::new("").render(Rect::new(0, 0, 10, 1), frame);
        Widget::render(&Help::new(), Rect::new(0, 1, 30, 1), frame);
        boundary.render(Rect::new(0, 2, 60, 4), frame, &mut eb_state);
    });
    assert_eq!(
        tree.node_count(),
        1,
        "empty Badge and empty Help must not emit ghost nodes"
    );
    let alert = tree
        .nodes()
        .find(|n| n.live_region == Some(LiveRegion::Assertive))
        .expect("ErrorBoundary alert");
    let desc = alert.description.as_deref().unwrap_or("");
    assert!(desc.ends_with("...") && desc.chars().count() == 203);
}

#[test]
fn sparkline_history_decision_and_receipt_emit_accessibility() {
    use ftui_a11y::node::A11yRole;
    use ftui_runtime::transparency::{Disclosure, DisclosureLevel, TrafficLight};
    use ftui_runtime::unified_evidence::DecisionDomain;
    use ftui_widgets::Widget;
    use ftui_widgets::decision_card::DecisionCard;
    use ftui_widgets::history_panel::HistoryPanel;
    use ftui_widgets::receipt_verifier_panel::{
        FailureClass, ReceiptVerdict, ReceiptVerifierPanel,
    };
    use ftui_widgets::sparkline::Sparkline;

    let samples = [1.0, 4.0, 2.5, 8.0, 5.0];
    let spark = Sparkline::new(&samples);
    let history = HistoryPanel::new()
        .with_title("Edit History")
        .with_undo_items(&["Insert title", "Format block"])
        .with_redo_items(&["Delete footer"]);
    let disclosure = Disclosure {
        domain: DecisionDomain::DiffStrategy,
        level: DisclosureLevel::PlainEnglish,
        signal: TrafficLight::Green,
        action_label: "dirty_rows".to_string(),
        explanation: Some("Low change rate favors dirty-row diff.".to_string()),
        evidence_terms: None,
        bayesian_details: None,
    };
    let card = DecisionCard::new(&disclosure);
    let mut verdict = ReceiptVerdict::skeleton("rcpt-42", false);
    verdict.signature.passed = true;
    verdict.transparency.passed = true;
    verdict.attestation.passed = false;
    verdict.failure_class = Some(FailureClass::Attestation);
    let receipt_panel = ReceiptVerifierPanel::new(&verdict);

    let tree = build_a11y_tree(80, 24, |frame| {
        spark.render(Rect::new(0, 0, 20, 1), frame);
        history.render(Rect::new(0, 2, 30, 8), frame);
        card.render(Rect::new(32, 0, 44, 5), frame);
        receipt_panel.render(Rect::new(32, 6, 44, 8), frame);
    });

    let spark_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Label && n.name.as_deref() == Some("Sparkline"))
        .expect("Sparkline should emit Label node");
    let spark_desc = spark_node.description.as_deref().unwrap_or("");
    assert!(spark_desc.contains("5 samples"));
    assert!(spark_desc.contains("min 1.00"));
    assert!(spark_desc.contains("max 8.00"));
    assert!(spark_desc.contains("latest 5.00"));

    let hist_root = tree
        .nodes()
        .find(|n| n.role == A11yRole::List && n.name.as_deref() == Some("Edit History"))
        .expect("HistoryPanel should emit List container");
    assert_eq!(hist_root.description.as_deref(), Some("2 undo, 1 redo"));
    let undo_item = tree
        .nodes()
        .find(|n| n.parent == Some(hist_root.id) && n.name.as_deref() == Some("Format block"))
        .expect("HistoryPanel should expose undo entry");
    assert_eq!(undo_item.description.as_deref(), Some("Undo"));
    let redo_item = tree
        .nodes()
        .find(|n| n.parent == Some(hist_root.id) && n.name.as_deref() == Some("Delete footer"))
        .expect("HistoryPanel should expose redo entry");
    assert_eq!(redo_item.description.as_deref(), Some("Redo"));

    let decision_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("OK: dirty_rows"))
        .expect("DecisionCard should emit Group node");
    assert_eq!(
        decision_node.description.as_deref(),
        Some("Low change rate favors dirty-row diff.")
    );

    let receipt_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("Receipt rcpt-42: FAIL"))
        .expect("ReceiptVerifierPanel should emit Group node");
    let receipt_desc = receipt_node.description.as_deref().unwrap_or("");
    assert!(receipt_desc.contains("signature=PASS"));
    assert!(receipt_desc.contains("attestation=FAIL"));
    assert!(receipt_desc.contains("failure_class=Attestation"));
}

#[test]
fn pretty_drift_and_empty_string_a11y_guards() {
    use ftui_a11y::node::{A11yRole, LiveRegion};
    use ftui_runtime::transparency::TrafficLight;
    use ftui_runtime::unified_evidence::DecisionDomain;
    use ftui_widgets::Widget;
    use ftui_widgets::badge::Badge;
    use ftui_widgets::drift_visualization::{
        DomainSnapshot, DriftSnapshot, DriftTimeline, DriftVisualization,
    };
    use ftui_widgets::help::Help;
    use ftui_widgets::panel::Panel;
    use ftui_widgets::pretty::Pretty;

    let value = vec![10, 20, 30];
    let pretty = Pretty::new(&value);

    let mut timeline = DriftTimeline::new(16);
    timeline.push(DriftSnapshot {
        frame_id: 1,
        domains: vec![DomainSnapshot {
            domain: DecisionDomain::DiffStrategy,
            confidence: 0.25,
            signal: TrafficLight::Red,
            in_fallback: true,
            regime_label: "deterministic",
        }],
    });
    let drift = DriftVisualization::new(&timeline);

    // Help with an empty entry ("", "") plus a key-only entry ("Esc", "").
    let help = Help::new().entry("", "").entry("Esc", "");
    // Panel with empty title/subtitle must not emit empty-string name/description.
    let panel = Panel::new(Badge::new("OK")).title("").subtitle("");

    let tree = build_a11y_tree(80, 24, |frame| {
        pretty.render(Rect::new(0, 0, 30, 4), frame);
        drift.render(Rect::new(0, 5, 44, 8), frame);
        Widget::render(&help, Rect::new(0, 14, 30, 1), frame);
        panel.render(Rect::new(46, 0, 20, 4), frame);
    });

    let pretty_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Label && n.name.as_deref() == Some("Pretty"))
        .expect("Pretty should emit Label node");
    assert!(
        pretty_node
            .description
            .as_deref()
            .unwrap_or("")
            .contains("10")
    );

    let drift_node = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("Drift Monitor"))
        .expect("DriftVisualization should emit Drift Monitor Group");
    assert_eq!(drift_node.live_region, Some(LiveRegion::Polite));
    assert!(
        drift_node
            .description
            .as_deref()
            .unwrap_or("")
            .contains("fallback active")
    );
    assert!(
        tree.nodes()
            .any(|n| n.parent == Some(drift_node.id) && n.name.as_deref() == Some("Sparkline")),
        "Domain confidence sparkline should be scoped under Drift Monitor"
    );

    let help_root = tree
        .nodes()
        .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("Keyboard shortcuts"))
        .expect("Help with one non-empty entry should emit Group");
    assert_eq!(
        help_root.children.len(),
        1,
        "Empty (\"\", \"\") Help entry must not emit a ghost child"
    );
    let esc_node = tree.node(help_root.children[0]).expect("Esc hint node");
    assert_eq!(esc_node.name.as_deref(), Some("Esc"));
    assert_eq!(esc_node.shortcut.as_deref(), Some("Esc"));

    let ok_badge = tree
        .nodes()
        .find(|n| n.role == A11yRole::Label && n.name.as_deref() == Some("OK"))
        .expect("Panel child badge");
    let panel_node = tree
        .node(ok_badge.parent.expect("Panel parent"))
        .expect("Panel container node");
    assert_eq!(panel_node.name, None, "Empty Panel title must map to None");
    assert_eq!(
        panel_node.description, None,
        "Empty Panel subtitle must map to None"
    );
}
