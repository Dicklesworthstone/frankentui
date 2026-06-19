//! Comprehensive tests for the ftui-a11y crate.

use ftui_a11y::Accessible;
use ftui_a11y::node::{A11yNodeInfo, A11yRole, A11yState, LiveRegion};
use ftui_a11y::tree::{
    A11yChange, A11yTree, A11yTreeBuilder, AnnouncementReason, ScreenReaderPolicy,
};
use ftui_core::geometry::Rect;

// ── Helper: a toy widget that implements Accessible ────────────────────

struct FakeButton {
    id: u64,
    label: String,
}

impl Accessible for FakeButton {
    fn accessibility_nodes(&self, area: Rect) -> Vec<A11yNodeInfo> {
        vec![A11yNodeInfo::new(self.id, A11yRole::Button, area).with_name(&self.label)]
    }
}

struct FakeList {
    id: u64,
    items: Vec<(u64, String)>,
}

impl Accessible for FakeList {
    fn accessibility_nodes(&self, area: Rect) -> Vec<A11yNodeInfo> {
        let child_ids: Vec<u64> = self.items.iter().map(|(id, _)| *id).collect();
        let mut nodes =
            vec![A11yNodeInfo::new(self.id, A11yRole::List, area).with_children(child_ids)];
        for (i, (item_id, label)) in self.items.iter().enumerate() {
            let item_rect = Rect::new(area.x, area.y + i as u16, area.width, 1);
            nodes.push(
                A11yNodeInfo::new(*item_id, A11yRole::ListItem, item_rect)
                    .with_name(label)
                    .with_parent(self.id),
            );
        }
        nodes
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Node tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn node_new_has_correct_defaults() {
    let node = A11yNodeInfo::new(42, A11yRole::Button, Rect::new(1, 2, 10, 3));
    assert_eq!(node.id, 42);
    assert_eq!(node.role, A11yRole::Button);
    assert_eq!(node.bounds, Rect::new(1, 2, 10, 3));
    assert!(node.name.is_none());
    assert!(node.description.is_none());
    assert!(node.shortcut.is_none());
    assert!(node.children.is_empty());
    assert!(node.parent.is_none());
    assert!(node.live_region.is_none());
    assert!(!node.state.focused);
    assert!(!node.state.disabled);
    assert!(node.state.checked.is_none());
}

#[test]
fn node_builder_methods_chain() {
    let node = A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 5, 1))
        .with_name("Save")
        .with_description("Save the current file")
        .with_shortcut("Ctrl+S")
        .with_live_region(LiveRegion::Polite)
        .with_children(vec![10, 11])
        .with_parent(0);

    assert_eq!(node.name.as_deref(), Some("Save"));
    assert_eq!(node.description.as_deref(), Some("Save the current file"));
    assert_eq!(node.shortcut.as_deref(), Some("Ctrl+S"));
    assert_eq!(node.live_region, Some(LiveRegion::Polite));
    assert_eq!(node.children, vec![10, 11]);
    assert_eq!(node.parent, Some(0));
}

#[test]
fn state_default_is_all_unset() {
    let state = A11yState::default();
    assert!(!state.focused);
    assert!(!state.disabled);
    assert!(state.checked.is_none());
    assert!(state.expanded.is_none());
    assert!(!state.selected);
    assert!(!state.readonly);
    assert!(!state.required);
    assert!(!state.busy);
    assert!(state.value_now.is_none());
    assert!(state.value_min.is_none());
    assert!(state.value_max.is_none());
    assert!(state.value_text.is_none());
}

// ── Role tests ─────────────────────────────────────────────────────────

#[test]
fn role_interactive_returns_true_for_controls() {
    assert!(A11yRole::Button.is_interactive());
    assert!(A11yRole::TextInput.is_interactive());
    assert!(A11yRole::Checkbox.is_interactive());
    assert!(A11yRole::RadioButton.is_interactive());
    assert!(A11yRole::Slider.is_interactive());
    assert!(A11yRole::Tab.is_interactive());
    assert!(A11yRole::MenuItem.is_interactive());
}

#[test]
fn role_interactive_returns_false_for_containers() {
    assert!(!A11yRole::Window.is_interactive());
    assert!(!A11yRole::Dialog.is_interactive());
    assert!(!A11yRole::Label.is_interactive());
    assert!(!A11yRole::List.is_interactive());
    assert!(!A11yRole::Table.is_interactive());
    assert!(!A11yRole::Group.is_interactive());
    assert!(!A11yRole::Presentation.is_interactive());
    assert!(!A11yRole::Separator.is_interactive());
    assert!(!A11yRole::Toolbar.is_interactive());
    assert!(!A11yRole::ProgressBar.is_interactive());
}

#[test]
fn role_display_format() {
    assert_eq!(format!("{}", A11yRole::Button), "button");
    assert_eq!(format!("{}", A11yRole::TextInput), "textInput");
    assert_eq!(format!("{}", A11yRole::ProgressBar), "progressBar");
    assert_eq!(format!("{}", A11yRole::Presentation), "presentation");
}

#[test]
fn live_region_display_format() {
    assert_eq!(format!("{}", LiveRegion::Polite), "polite");
    assert_eq!(format!("{}", LiveRegion::Assertive), "assertive");
}

// ═══════════════════════════════════════════════════════════════════════
// Tree builder tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn builder_empty_produces_empty_tree() {
    let tree = A11yTreeBuilder::new().build();
    assert!(tree.is_empty());
    assert_eq!(tree.node_count(), 0);
    assert!(tree.root().is_none());
    assert!(tree.focused().is_none());
}

#[test]
fn builder_with_capacity_works() {
    let mut builder = A11yTreeBuilder::with_capacity(16);
    builder.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    let tree = builder.build();
    assert_eq!(tree.node_count(), 1);
}

#[test]
fn builder_add_and_retrieve_nodes() {
    let mut builder = A11yTreeBuilder::new();
    builder.add_node(
        A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24))
            .with_name("App")
            .with_children(vec![2, 3]),
    );
    builder.add_node(
        A11yNodeInfo::new(2, A11yRole::Button, Rect::new(5, 10, 10, 1))
            .with_name("OK")
            .with_parent(1),
    );
    builder.add_node(
        A11yNodeInfo::new(3, A11yRole::Button, Rect::new(20, 10, 10, 1))
            .with_name("Cancel")
            .with_parent(1),
    );
    builder.set_root(1);
    builder.set_focused(Some(2));

    let tree = builder.build();

    assert_eq!(tree.node_count(), 3);
    assert_eq!(tree.root().unwrap().name.as_deref(), Some("App"));
    assert_eq!(tree.root_id(), Some(1));
    assert_eq!(tree.focused().unwrap().name.as_deref(), Some("OK"));
    assert_eq!(tree.focused_id(), Some(2));

    let node2 = tree.node(2).unwrap();
    assert_eq!(node2.role, A11yRole::Button);
    assert_eq!(node2.parent, Some(1));
}

#[test]
fn builder_replace_node_with_same_id() {
    let mut builder = A11yTreeBuilder::new();
    builder.add_node(A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 5, 1)).with_name("A"));
    builder.add_node(A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 5, 1)).with_name("B"));
    let tree = builder.build();
    assert_eq!(tree.node_count(), 1);
    assert_eq!(tree.node(1).unwrap().name.as_deref(), Some("B"));
}

#[test]
fn builder_default_trait() {
    let builder = A11yTreeBuilder::default();
    let tree = builder.build();
    assert!(tree.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════
// Tree query tests
// ═══════════════════════════════════════════════════════════════════════

fn build_sample_tree() -> A11yTree {
    let mut b = A11yTreeBuilder::new();
    b.add_node(
        A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24))
            .with_name("Root")
            .with_children(vec![2, 3]),
    );
    b.add_node(
        A11yNodeInfo::new(2, A11yRole::Group, Rect::new(0, 0, 40, 24))
            .with_name("Sidebar")
            .with_parent(1)
            .with_children(vec![4]),
    );
    b.add_node(
        A11yNodeInfo::new(3, A11yRole::Group, Rect::new(40, 0, 40, 24))
            .with_name("Main")
            .with_parent(1),
    );
    b.add_node(
        A11yNodeInfo::new(4, A11yRole::Button, Rect::new(5, 5, 10, 1))
            .with_name("Click Me")
            .with_parent(2),
    );
    b.set_root(1);
    b.set_focused(Some(4));
    b.build()
}

#[test]
fn tree_children_of() {
    let tree = build_sample_tree();
    let children = tree.children_of(1);
    assert_eq!(children.len(), 2);
    let names: Vec<_> = children
        .iter()
        .map(|n| n.name.as_deref().unwrap())
        .collect();
    assert!(names.contains(&"Sidebar"));
    assert!(names.contains(&"Main"));
}

#[test]
fn tree_children_of_leaf() {
    let tree = build_sample_tree();
    let children = tree.children_of(4);
    assert!(children.is_empty());
}

#[test]
fn tree_children_of_nonexistent() {
    let tree = build_sample_tree();
    let children = tree.children_of(999);
    assert!(children.is_empty());
}

#[test]
fn tree_ancestors() {
    let tree = build_sample_tree();
    let path = tree.ancestors(4);
    assert_eq!(path, vec![4, 2, 1]);
}

#[test]
fn tree_ancestors_of_root() {
    let tree = build_sample_tree();
    let path = tree.ancestors(1);
    assert_eq!(path, vec![1]);
}

#[test]
fn tree_ancestors_of_nonexistent() {
    let tree = build_sample_tree();
    let path = tree.ancestors(999);
    assert!(path.is_empty());
}

#[test]
fn tree_nodes_iterator() {
    let tree = build_sample_tree();
    let ids: Vec<u64> = {
        let mut ids: Vec<_> = tree.nodes().map(|n| n.id).collect();
        ids.sort();
        ids
    };
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[test]
fn tree_empty_default() {
    let tree = A11yTree::default();
    assert!(tree.is_empty());
    assert!(tree.root().is_none());
    assert!(tree.focused().is_none());
}

// ═══════════════════════════════════════════════════════════════════════
// Diff tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn diff_identical_trees_is_empty() {
    let tree = build_sample_tree();

    // Rebuild an identical tree.
    let tree2 = build_sample_tree();

    let diff = tree2.diff(&tree);
    assert!(diff.is_empty());
}

#[test]
fn diff_detects_added_nodes() {
    let tree1 = build_sample_tree();

    // Add node 5 in tree2.
    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        b.add_node(node.clone());
    }
    b.add_node(A11yNodeInfo::new(5, A11yRole::Label, Rect::new(0, 23, 20, 1)).with_name("Status"));
    b.set_root(1);
    b.set_focused(Some(4));
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    assert!(diff.added.contains(&5));
    assert!(diff.removed.is_empty());
}

#[test]
fn diff_detects_removed_nodes() {
    let tree1 = build_sample_tree();

    // Build tree2 without node 4.
    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        if node.id != 4 {
            b.add_node(node.clone());
        }
    }
    b.set_root(1);
    b.set_focused(None);
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    assert!(diff.removed.contains(&4));
    assert!(diff.added.is_empty());
}

#[test]
fn diff_detects_name_change() {
    let tree1 = build_sample_tree();

    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        if node.id == 4 {
            let mut n = node.clone();
            n.name = Some("Don't Click Me".to_owned());
            b.add_node(n);
        } else {
            b.add_node(node.clone());
        }
    }
    b.set_root(1);
    b.set_focused(Some(4));
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    let changed_entry = diff.changed.iter().find(|(id, _)| *id == 4);
    assert!(changed_entry.is_some());
    let changes = &changed_entry.unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::NameChanged {
            old: Some(old),
            new: Some(new),
        } if old == "Click Me" && new == "Don't Click Me"
    )));
}

#[test]
fn diff_detects_role_change() {
    let tree1 = build_sample_tree();

    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        if node.id == 4 {
            let mut n = node.clone();
            n.role = A11yRole::Label;
            b.add_node(n);
        } else {
            b.add_node(node.clone());
        }
    }
    b.set_root(1);
    b.set_focused(Some(4));
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 4).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::RoleChanged {
            old: A11yRole::Button,
            new: A11yRole::Label,
        }
    )));
}

#[test]
fn diff_detects_bounds_change() {
    let tree1 = build_sample_tree();

    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        if node.id == 4 {
            let mut n = node.clone();
            n.bounds = Rect::new(10, 10, 20, 2);
            b.add_node(n);
        } else {
            b.add_node(node.clone());
        }
    }
    b.set_root(1);
    b.set_focused(Some(4));
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 4).unwrap().1;
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, A11yChange::BoundsChanged))
    );
}

#[test]
fn diff_detects_children_change() {
    let tree1 = build_sample_tree();

    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        if node.id == 1 {
            let mut n = node.clone();
            n.children = vec![2, 3, 5]; // added child 5
            b.add_node(n);
        } else {
            b.add_node(node.clone());
        }
    }
    b.add_node(A11yNodeInfo::new(5, A11yRole::Label, Rect::new(0, 0, 10, 1)).with_parent(1));
    b.set_root(1);
    b.set_focused(Some(4));
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, A11yChange::ChildrenChanged))
    );
}

#[test]
fn diff_detects_state_change_focused() {
    let tree1 = build_sample_tree();

    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        if node.id == 4 {
            let mut n = node.clone();
            n.state.disabled = true;
            b.add_node(n);
        } else {
            b.add_node(node.clone());
        }
    }
    b.set_root(1);
    b.set_focused(Some(4));
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 4).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::StateChanged { field, description }
        if field == "disabled" && description == "true"
    )));
}

#[test]
fn diff_detects_state_change_checked() {
    let mut b1 = A11yTreeBuilder::new();
    let mut cb =
        A11yNodeInfo::new(1, A11yRole::Checkbox, Rect::new(0, 0, 3, 1)).with_name("Accept");
    cb.state.checked = Some(false);
    b1.add_node(cb);
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    let mut cb2 =
        A11yNodeInfo::new(1, A11yRole::Checkbox, Rect::new(0, 0, 3, 1)).with_name("Accept");
    cb2.state.checked = Some(true);
    b2.add_node(cb2);
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::StateChanged { field, .. } if field == "checked"
    )));
}

#[test]
fn diff_detects_state_change_value() {
    let mut b1 = A11yTreeBuilder::new();
    let mut slider =
        A11yNodeInfo::new(1, A11yRole::Slider, Rect::new(0, 0, 20, 1)).with_name("Volume");
    slider.state.value_now = Some(50.0);
    slider.state.value_min = Some(0.0);
    slider.state.value_max = Some(100.0);
    slider.state.value_text = Some("50%".to_owned());
    b1.add_node(slider);
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    let mut slider2 =
        A11yNodeInfo::new(1, A11yRole::Slider, Rect::new(0, 0, 20, 1)).with_name("Volume");
    slider2.state.value_now = Some(75.0);
    slider2.state.value_min = Some(0.0);
    slider2.state.value_max = Some(100.0);
    slider2.state.value_text = Some("75%".to_owned());
    b2.add_node(slider2);
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::StateChanged { field, .. } if field == "value_now"
    )));
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::StateChanged { field, .. } if field == "value_text"
    )));
}

#[test]
fn diff_detects_live_region_change() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(
        A11yNodeInfo::new(1, A11yRole::Label, Rect::new(0, 0, 40, 1))
            .with_name("Status")
            .with_live_region(LiveRegion::Polite),
    );
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(
        A11yNodeInfo::new(1, A11yRole::Label, Rect::new(0, 0, 40, 1))
            .with_name("Status")
            .with_live_region(LiveRegion::Assertive),
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::LiveRegionChanged {
            old: Some(LiveRegion::Polite),
            new: Some(LiveRegion::Assertive),
        }
    )));
}

#[test]
fn diff_detects_focus_change() {
    let tree1 = build_sample_tree(); // focused = Some(4)

    let mut b = A11yTreeBuilder::new();
    for node in tree1.nodes() {
        b.add_node(node.clone());
    }
    b.set_root(1);
    b.set_focused(Some(3)); // changed focus
    let tree2 = b.build();

    let diff = tree2.diff(&tree1);
    assert_eq!(diff.focus_changed, Some((Some(4), Some(3))));
}

#[test]
fn diff_detects_focus_gained() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Button,
        Rect::new(0, 0, 5, 1),
    ));
    b1.set_root(1);
    // no focus set
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Button,
        Rect::new(0, 0, 5, 1),
    ));
    b2.set_root(1);
    b2.set_focused(Some(1));
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    assert_eq!(diff.focus_changed, Some((None, Some(1))));
}

#[test]
fn diff_detects_focus_lost() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Button,
        Rect::new(0, 0, 5, 1),
    ));
    b1.set_root(1);
    b1.set_focused(Some(1));
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Button,
        Rect::new(0, 0, 5, 1),
    ));
    b2.set_root(1);
    // no focus
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    assert_eq!(diff.focus_changed, Some((Some(1), None)));
}

#[test]
fn diff_empty_to_populated() {
    let empty = A11yTree::empty();
    let tree = build_sample_tree();
    let diff = tree.diff(&empty);
    assert_eq!(diff.added.len(), 4);
    assert!(diff.removed.is_empty());
}

#[test]
fn diff_populated_to_empty() {
    let tree = build_sample_tree();
    let empty = A11yTree::empty();
    let diff = empty.diff(&tree);
    assert!(diff.added.is_empty());
    assert_eq!(diff.removed.len(), 4);
}

#[test]
fn diff_orders_node_ids_deterministically() {
    let empty = A11yTree::empty();
    let mut added_builder = A11yTreeBuilder::new();
    for id in [30, 10, 20] {
        added_builder.add_node(A11yNodeInfo::new(
            id,
            A11yRole::Button,
            Rect::new(0, 0, 5, 1),
        ));
    }
    let added_tree = added_builder.build();
    let added_diff = added_tree.diff(&empty);
    assert_eq!(added_diff.added, vec![10, 20, 30]);

    let removed_diff = empty.diff(&added_tree);
    assert_eq!(removed_diff.removed, vec![10, 20, 30]);

    let mut changed_builder = A11yTreeBuilder::new();
    for id in [30, 10, 20] {
        changed_builder.add_node(
            A11yNodeInfo::new(id, A11yRole::Button, Rect::new(0, 0, 5, 1)).with_name("changed"),
        );
    }
    let changed_tree = changed_builder.build();
    let changed_diff = changed_tree.diff(&added_tree);
    let changed_ids: Vec<u64> = changed_diff.changed.iter().map(|(id, _)| *id).collect();
    assert_eq!(changed_ids, vec![10, 20, 30]);
}

// ═══════════════════════════════════════════════════════════════════════
// Screen-reader mirror and announcement tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn screen_reader_mirror_uses_child_order_and_bounds_output() {
    let mut b = A11yTreeBuilder::new();
    b.add_node(
        A11yNodeInfo::new(10, A11yRole::Window, Rect::new(0, 0, 80, 24))
            .with_name("App")
            .with_children(vec![30, 40, 20]),
    );
    b.add_node(
        A11yNodeInfo::new(20, A11yRole::Button, Rect::new(0, 2, 10, 1))
            .with_name("Cancel")
            .with_parent(10),
    );
    b.add_node(
        A11yNodeInfo::new(30, A11yRole::Button, Rect::new(0, 1, 10, 1))
            .with_name("Save")
            .with_parent(10),
    );
    b.add_node(
        A11yNodeInfo::new(40, A11yRole::Presentation, Rect::new(0, 1, 1, 1)).with_parent(10),
    );
    b.set_root(10);
    b.set_focused(Some(30));
    let tree = b.build();

    let mirror = tree.screen_reader_mirror(ScreenReaderPolicy {
        max_mirror_nodes: 2,
        max_announcements: 8,
        max_text_chars: 80,
    });

    assert_eq!(
        mirror.lines,
        vec![
            "window: App".to_owned(),
            "  button: Save. focused".to_owned()
        ]
    );
    assert_eq!(mirror.omitted_nodes, 1);
    assert_eq!(mirror.text(), "window: App\n  button: Save. focused");
}

#[test]
fn screen_reader_mirror_appends_disconnected_nodes_by_id() {
    let mut b = A11yTreeBuilder::new();
    b.add_node(A11yNodeInfo::new(
        100,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    b.add_node(A11yNodeInfo::new(3, A11yRole::Label, Rect::new(0, 1, 20, 1)).with_name("Three"));
    b.add_node(A11yNodeInfo::new(2, A11yRole::Label, Rect::new(0, 2, 20, 1)).with_name("Two"));
    b.set_root(100);
    let tree = b.build();

    let mirror = tree.screen_reader_mirror(ScreenReaderPolicy::default());

    assert_eq!(
        mirror.lines,
        vec![
            "window".to_owned(),
            "label: Two".to_owned(),
            "label: Three".to_owned()
        ]
    );
}

#[test]
fn screen_reader_announcements_include_focus_and_live_content_changes() {
    let mut before = A11yTreeBuilder::new();
    before
        .add_node(A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1)).with_name("Save"));
    before.add_node(
        A11yNodeInfo::new(2, A11yRole::Label, Rect::new(0, 1, 20, 1))
            .with_name("Loading")
            .with_live_region(LiveRegion::Polite),
    );
    before.set_root(1);
    let before = before.build();

    let mut after = A11yTreeBuilder::new();
    after
        .add_node(A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1)).with_name("Save"));
    after.add_node(
        A11yNodeInfo::new(2, A11yRole::Label, Rect::new(0, 1, 20, 1))
            .with_name("Loaded")
            .with_live_region(LiveRegion::Polite),
    );
    after.set_root(1);
    after.set_focused(Some(1));
    let after = after.build();

    let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());

    assert_eq!(batch.dropped_count, 0);
    assert_eq!(batch.announcements.len(), 2);
    assert_eq!(batch.announcements[0].node_id, Some(1));
    assert_eq!(
        batch.announcements[0].reason,
        AnnouncementReason::FocusChanged
    );
    assert_eq!(batch.announcements[0].text, "button: Save. focused");
    assert_eq!(batch.announcements[1].node_id, Some(2));
    assert_eq!(
        batch.announcements[1].reason,
        AnnouncementReason::LiveContentChanged
    );
    assert_eq!(batch.announcements[1].text, "label: Loaded");
}

#[test]
fn screen_reader_announcements_are_bounded_and_prioritize_assertive_live_regions() {
    let before = A11yTree::empty();
    let mut after = A11yTreeBuilder::new();
    after.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    for (id, region, name) in [
        (30, LiveRegion::Polite, "Announcement 30 with extra detail"),
        (10, LiveRegion::Polite, "Announcement 10 with extra detail"),
        (
            20,
            LiveRegion::Assertive,
            "Announcement 20 with extra detail",
        ),
        (
            40,
            LiveRegion::Assertive,
            "Announcement 40 with extra detail",
        ),
    ] {
        after.add_node(
            A11yNodeInfo::new(id, A11yRole::Label, Rect::new(0, 1, 20, 1))
                .with_name(name)
                .with_live_region(region),
        );
    }
    after.set_root(1);
    let after = after.build();

    let batch = after.screen_reader_announcements_since(
        &before,
        ScreenReaderPolicy {
            max_mirror_nodes: 128,
            max_announcements: 3,
            max_text_chars: 18,
        },
    );

    let node_ids: Vec<_> = batch
        .announcements
        .iter()
        .map(|announcement| announcement.node_id)
        .collect();
    assert_eq!(node_ids, vec![Some(20), Some(40), Some(10)]);
    assert_eq!(batch.dropped_count, 1);
    assert!(
        batch
            .announcements
            .iter()
            .all(|announcement| announcement.text.chars().count() <= 18)
    );
}

#[test]
fn screen_reader_announcements_skip_empty_live_regions_and_presentational_nodes() {
    let before = A11yTree::empty();
    let mut after = A11yTreeBuilder::new();
    after.add_node(
        A11yNodeInfo::new(1, A11yRole::Presentation, Rect::new(0, 0, 1, 1))
            .with_name("Decorative")
            .with_live_region(LiveRegion::Assertive),
    );
    after.add_node(
        A11yNodeInfo::new(2, A11yRole::Label, Rect::new(0, 1, 20, 1))
            .with_live_region(LiveRegion::Polite),
    );
    after.set_root(1);
    after.set_focused(Some(1));
    let after = after.build();

    let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
    assert!(batch.announcements.is_empty());
    assert_eq!(batch.dropped_count, 0);
}

// ═══════════════════════════════════════════════════════════════════════
// Accessible trait tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn accessible_button_returns_single_node() {
    let btn = FakeButton {
        id: 100,
        label: "Submit".to_owned(),
    };
    let nodes = btn.accessibility_nodes(Rect::new(5, 10, 12, 1));
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, 100);
    assert_eq!(nodes[0].role, A11yRole::Button);
    assert_eq!(nodes[0].name.as_deref(), Some("Submit"));
    assert_eq!(nodes[0].bounds, Rect::new(5, 10, 12, 1));
}

#[test]
fn accessible_list_returns_parent_and_children() {
    let list = FakeList {
        id: 200,
        items: vec![
            (201, "Alpha".into()),
            (202, "Beta".into()),
            (203, "Gamma".into()),
        ],
    };
    let nodes = list.accessibility_nodes(Rect::new(0, 0, 30, 10));
    assert_eq!(nodes.len(), 4); // 1 list + 3 items
    assert_eq!(nodes[0].role, A11yRole::List);
    assert_eq!(nodes[0].children, vec![201, 202, 203]);
    for node in nodes.iter().skip(1).take(3) {
        assert_eq!(node.role, A11yRole::ListItem);
        assert_eq!(node.parent, Some(200));
    }
}

#[test]
fn accessible_widget_integrates_with_tree_builder() {
    let btn = FakeButton {
        id: 1,
        label: "OK".to_owned(),
    };
    let area = Rect::new(0, 0, 5, 1);
    let nodes = btn.accessibility_nodes(area);

    let mut builder = A11yTreeBuilder::new();
    for node in nodes {
        builder.add_node(node);
    }
    builder.set_root(1);
    builder.set_focused(Some(1));
    let tree = builder.build();

    assert_eq!(tree.node_count(), 1);
    assert_eq!(tree.root().unwrap().role, A11yRole::Button);
    assert_eq!(tree.focused().unwrap().name.as_deref(), Some("OK"));
}

// ═══════════════════════════════════════════════════════════════════════
// Edge case tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn tree_focused_id_points_to_nonexistent_node() {
    let mut b = A11yTreeBuilder::new();
    b.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    b.set_root(1);
    b.set_focused(Some(999)); // node 999 doesn't exist
    let tree = b.build();

    assert_eq!(tree.focused_id(), Some(999));
    assert!(tree.focused().is_none()); // gracefully returns None
}

#[test]
fn tree_root_id_points_to_nonexistent_node() {
    let mut b = A11yTreeBuilder::new();
    b.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    b.set_root(999);
    let tree = b.build();

    assert_eq!(tree.root_id(), Some(999));
    assert!(tree.root().is_none());
}

#[test]
fn diff_multiple_state_changes_on_same_node() {
    let mut b1 = A11yTreeBuilder::new();
    let mut n = A11yNodeInfo::new(1, A11yRole::Checkbox, Rect::new(0, 0, 3, 1)).with_name("Option");
    n.state.checked = Some(false);
    n.state.disabled = false;
    n.state.focused = false;
    b1.add_node(n);
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    let mut n2 =
        A11yNodeInfo::new(1, A11yRole::Checkbox, Rect::new(0, 0, 3, 1)).with_name("Option");
    n2.state.checked = Some(true);
    n2.state.disabled = true;
    n2.state.focused = true;
    b2.add_node(n2);
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    // Should detect all three state changes.
    let fields: Vec<&str> = changes
        .iter()
        .filter_map(|c| match c {
            A11yChange::StateChanged { field, .. } => Some(field.as_str()),
            _ => None,
        })
        .collect();
    assert!(fields.contains(&"checked"));
    assert!(fields.contains(&"disabled"));
    assert!(fields.contains(&"focused"));
}

#[test]
fn diff_is_empty_predicate() {
    let tree = build_sample_tree();
    let tree2 = build_sample_tree();
    let diff = tree2.diff(&tree);
    assert!(diff.is_empty());
}

#[test]
fn all_roles_have_display() {
    // Ensure all variants produce non-empty strings.
    let roles = [
        A11yRole::Window,
        A11yRole::Dialog,
        A11yRole::Button,
        A11yRole::TextInput,
        A11yRole::Label,
        A11yRole::List,
        A11yRole::ListItem,
        A11yRole::Table,
        A11yRole::TableRow,
        A11yRole::TableCell,
        A11yRole::Checkbox,
        A11yRole::RadioButton,
        A11yRole::ProgressBar,
        A11yRole::Slider,
        A11yRole::Tab,
        A11yRole::TabPanel,
        A11yRole::Menu,
        A11yRole::MenuItem,
        A11yRole::Toolbar,
        A11yRole::ScrollBar,
        A11yRole::Separator,
        A11yRole::Group,
        A11yRole::Presentation,
    ];
    for role in &roles {
        let s = format!("{role}");
        assert!(!s.is_empty(), "Role {role:?} has empty Display");
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Diff tests for description, shortcut, and parent changes
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn diff_detects_description_change() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("Save")
            .with_description("Save the current file"),
    );
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("Save")
            .with_description("Save all open files"),
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::DescriptionChanged {
            old: Some(old),
            new: Some(new),
        } if old == "Save the current file" && new == "Save all open files"
    )));
}

#[test]
fn diff_detects_description_added() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1)).with_name("Save"));
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("Save")
            .with_description("Save the file"),
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::DescriptionChanged {
            old: None,
            new: Some(_),
        }
    )));
}

#[test]
fn diff_detects_shortcut_change() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("Save")
            .with_shortcut("Ctrl+S"),
    );
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("Save")
            .with_shortcut("Cmd+S"),
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::ShortcutChanged {
            old: Some(old),
            new: Some(new),
        } if old == "Ctrl+S" && new == "Cmd+S"
    )));
}

#[test]
fn diff_detects_shortcut_removed() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("Save")
            .with_shortcut("Ctrl+S"),
    );
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(
        A11yNodeInfo::new(1, A11yRole::Button, Rect::new(0, 0, 10, 1)).with_name("Save"),
        // no shortcut
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::ShortcutChanged {
            old: Some(_),
            new: None,
        }
    )));
}

#[test]
fn diff_detects_parent_change() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(
        A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24)).with_children(vec![2]),
    );
    b1.add_node(
        A11yNodeInfo::new(2, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("OK")
            .with_parent(1),
    );
    b1.add_node(A11yNodeInfo::new(
        3,
        A11yRole::Group,
        Rect::new(0, 0, 40, 24),
    ));
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    b2.add_node(
        A11yNodeInfo::new(2, A11yRole::Button, Rect::new(0, 0, 10, 1))
            .with_name("OK")
            .with_parent(3), // reparented from 1 to 3
    );
    b2.add_node(
        A11yNodeInfo::new(3, A11yRole::Group, Rect::new(0, 0, 40, 24)).with_children(vec![2]),
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 2).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::ParentChanged {
            old: Some(1),
            new: Some(3),
        }
    )));
}

#[test]
fn diff_detects_parent_removed() {
    let mut b1 = A11yTreeBuilder::new();
    b1.add_node(
        A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24)).with_children(vec![2]),
    );
    b1.add_node(A11yNodeInfo::new(2, A11yRole::Button, Rect::new(0, 0, 10, 1)).with_parent(1));
    b1.set_root(1);
    let tree1 = b1.build();

    let mut b2 = A11yTreeBuilder::new();
    b2.add_node(A11yNodeInfo::new(
        1,
        A11yRole::Window,
        Rect::new(0, 0, 80, 24),
    ));
    b2.add_node(
        A11yNodeInfo::new(2, A11yRole::Button, Rect::new(0, 0, 10, 1)),
        // no parent
    );
    b2.set_root(1);
    let tree2 = b2.build();

    let diff = tree2.diff(&tree1);
    let changes = &diff.changed.iter().find(|(id, _)| *id == 2).unwrap().1;
    assert!(changes.iter().any(|c| matches!(
        c,
        A11yChange::ParentChanged {
            old: Some(1),
            new: None,
        }
    )));
}

// ═══════════════════════════════════════════════════════════════════════
// with_state builder method test
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn node_with_state_builder() {
    let state = A11yState {
        focused: true,
        disabled: false,
        checked: Some(true),
        ..A11yState::default()
    };
    let node = A11yNodeInfo::new(1, A11yRole::Checkbox, Rect::new(0, 0, 3, 1))
        .with_name("Accept")
        .with_state(state);

    assert!(node.state.focused);
    assert!(!node.state.disabled);
    assert_eq!(node.state.checked, Some(true));
}

// ═══════════════════════════════════════════════════════════════════════
// Cycle protection in ancestors()
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn ancestors_cycle_protection() {
    // Build a tree with a cyclic parent chain: 1 -> 2 -> 1.
    let mut b = A11yTreeBuilder::new();
    b.add_node(
        A11yNodeInfo::new(1, A11yRole::Group, Rect::new(0, 0, 80, 24))
            .with_parent(2)
            .with_children(vec![2]),
    );
    b.add_node(
        A11yNodeInfo::new(2, A11yRole::Group, Rect::new(0, 0, 40, 12))
            .with_parent(1)
            .with_children(vec![1]),
    );
    b.set_root(1);
    let tree = b.build();

    // Without cycle protection this would loop forever.
    // With protection it should stop after visiting each node once.
    let path = tree.ancestors(1);
    assert!(path.len() <= 2, "cycle should be broken, got {:?}", path);
    assert!(path.contains(&1));
    assert!(path.contains(&2));
}

#[test]
fn ancestors_self_cycle_protection() {
    // A node that is its own parent.
    let mut b = A11yTreeBuilder::new();
    b.add_node(A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24)).with_parent(1));
    b.set_root(1);
    let tree = b.build();

    let path = tree.ancestors(1);
    assert_eq!(path, vec![1], "self-cycle should stop after one visit");
}

// ═══════════════════════════════════════════════════════════════════════
// Reduced-motion / high-contrast preference integration (bd-2vr05.7.4)
//
// These exercise the preference subsystem end-to-end against the real
// announcement pipeline: a live region whose value churns rapidly (a progress
// bar updating frame-by-frame) is the canonical "motion" the reduced-motion
// flag must tame on the assistive-tech channel.
// ═══════════════════════════════════════════════════════════════════════

use ftui_a11y::preferences::{AccessibilityPreferences, ContrastProfile, MotionProfile};

/// Build a tree with a single progress bar live region at the given percent.
fn progress_tree(percent: u8, region: LiveRegion) -> A11yTree {
    let mut b = A11yTreeBuilder::new();
    b.add_node(
        A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24)).with_children(vec![2]),
    );
    let state = A11yState {
        value_now: Some(f64::from(percent)),
        value_text: Some(format!("{percent}%")),
        ..A11yState::default()
    };
    b.add_node(
        A11yNodeInfo::new(2, A11yRole::ProgressBar, Rect::new(0, 1, 40, 1))
            .with_name("Download")
            .with_parent(1)
            .with_state(state)
            .with_live_region(region),
    );
    b.set_root(1);
    b.build()
}

#[test]
fn preferences_full_motion_leaves_progress_announcements_untouched() {
    let prefs = AccessibilityPreferences::none();
    let motion = prefs.motion_profile();

    let mut prev = progress_tree(10, LiveRegion::Assertive);
    let mut emitted = 0usize;
    // Simulate three frames of progress churn; with full motion every change
    // is announced.
    for pct in [40u8, 70, 100] {
        let next = progress_tree(pct, LiveRegion::Assertive);
        let batch = next.screen_reader_announcements_since(&prev, ScreenReaderPolicy::default());
        let filtered = motion.filter_announcements(&batch);
        assert_eq!(filtered.coalesced_count, 0);
        assert_eq!(filtered.downgraded_count, 0);
        emitted += filtered.announcements.len();
        prev = next;
    }
    assert_eq!(emitted, 3, "every frame announces under full motion");
}

#[test]
fn preferences_reduced_motion_downgrades_progress_churn() {
    let prefs = AccessibilityPreferences::none().with_reduced_motion(true);
    let motion = prefs.motion_profile();

    let prev = progress_tree(10, LiveRegion::Assertive);
    let next = progress_tree(55, LiveRegion::Assertive);
    let batch = next.screen_reader_announcements_since(&prev, ScreenReaderPolicy::default());
    assert_eq!(
        batch.announcements.len(),
        1,
        "value change is announced once"
    );
    assert_eq!(batch.announcements[0].urgency, LiveRegion::Assertive);

    let filtered = motion.filter_announcements(&batch);
    assert_eq!(filtered.announcements.len(), 1);
    // Reduced motion downgrades the interrupting assertive churn to polite.
    assert_eq!(filtered.announcements[0].urgency, LiveRegion::Polite);
    assert_eq!(filtered.downgraded_count, 1);
}

#[test]
fn preferences_high_contrast_is_orthogonal_to_announcements() {
    // High contrast must NOT alter assistive-tech text output — it is a visual
    // channel concern surfaced via the contrast profile, not the mirror.
    let prefs = AccessibilityPreferences::none().with_high_contrast(true);
    let motion = prefs.motion_profile();
    assert_eq!(motion, MotionProfile::FULL);

    let prev = progress_tree(10, LiveRegion::Polite);
    let next = progress_tree(20, LiveRegion::Polite);
    let plain = next.screen_reader_announcements_since(&prev, ScreenReaderPolicy::default());
    let filtered = motion.filter_announcements(&plain);
    assert_eq!(filtered.announcements, plain.announcements);

    let contrast = prefs.contrast_profile();
    assert_eq!(
        contrast.min_contrast_ratio_x100,
        ContrastProfile::WCAG_AAA_X100
    );
    assert!(contrast.drop_dim);
}

#[test]
fn preferences_jsonl_round_trips_through_capability_profile() {
    // A fixed capability profile (media-query booleans) must produce a stable,
    // greppable log line for deterministic triage.
    let prefs = AccessibilityPreferences::from_media_queries(true, true, false);
    let line = prefs.to_jsonl("a11y-frame-0042");
    assert_eq!(line, prefs.to_jsonl("a11y-frame-0042"));
    assert!(line.contains("\"reduced_motion\":true"));
    assert!(line.contains("\"high_contrast\":true"));
    assert!(line.contains("\"forced_colors\":false"));
    assert!(line.contains("\"announcement_coalescing\":true"));
    assert!(line.contains("\"min_contrast_ratio_x100\":700"));
}
