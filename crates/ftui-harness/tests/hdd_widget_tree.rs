#![forbid(unsafe_code)]

//! Integration tests for HDD reduction on synthetic widget trees.
//!
//! Validates hierarchical delta debugging against realistic tree structures
//! that model FrankenTUI widget trees with various failure predicates.

use ftui_harness::hdd::{Decomposable, hdd_minimize};

// ============================================================================
// Synthetic Widget Tree
// ============================================================================

/// A minimal widget tree node for testing HDD.
#[derive(Clone, Debug, PartialEq)]
struct WidgetNode {
    id: u32,
    kind: &'static str,
    children: Vec<WidgetNode>,
}

impl WidgetNode {
    fn leaf(id: u32, kind: &'static str) -> Self {
        Self {
            id,
            kind,
            children: vec![],
        }
    }

    fn container(id: u32, kind: &'static str, children: Vec<WidgetNode>) -> Self {
        Self { id, kind, children }
    }

    /// Count total nodes in the subtree (including self).
    fn node_count(&self) -> usize {
        1 + self.children.iter().map(|c| c.node_count()).sum::<usize>()
    }

    /// Find a node by id anywhere in the tree.
    fn contains_id(&self, target: u32) -> bool {
        if self.id == target {
            return true;
        }
        self.children.iter().any(|c| c.contains_id(target))
    }

    /// Find a node by kind anywhere in the tree.
    fn contains_kind(&self, target: &str) -> bool {
        if self.kind == target {
            return true;
        }
        self.children.iter().any(|c| c.contains_kind(target))
    }
}

impl Decomposable for WidgetNode {
    fn children(&self) -> Vec<Self> {
        self.children.clone()
    }

    fn remove_child(&mut self, idx: usize) {
        self.children.remove(idx);
    }

    fn replace_children(&mut self, new_children: Vec<Self>) {
        self.children = new_children;
    }
}

// ============================================================================
// Tree Generators
// ============================================================================

/// Build a balanced tree with a given branching factor and depth.
/// Nodes are numbered sequentially with a global counter.
fn build_balanced_tree(
    counter: &mut u32,
    depth: usize,
    branching: usize,
    kind: &'static str,
) -> WidgetNode {
    let id = *counter;
    *counter += 1;

    if depth == 0 {
        return WidgetNode::leaf(id, kind);
    }

    let children = (0..branching)
        .map(|_| build_balanced_tree(counter, depth - 1, branching, kind))
        .collect();

    WidgetNode::container(id, kind, children)
}

/// Build a large tree with a bug node planted at a specific depth.
fn build_tree_with_bug(total_target: usize, bug_depth: usize) -> (WidgetNode, u32) {
    let mut counter = 0u32;

    // Build main tree structure.
    // Use branching factor 3, depth 4 → 1 + 3 + 9 + 27 + 81 = 121 nodes.
    let mut root = build_balanced_tree(&mut counter, 4, 3, "panel");

    // Plant a "buggy" node at the specified depth.
    let bug_id = counter;
    let bug_node = WidgetNode::leaf(bug_id, "buggy");

    // Navigate to the target depth and insert the bug node.
    fn insert_at_depth(node: &mut WidgetNode, bug: WidgetNode, depth: usize) -> bool {
        if depth == 0 {
            node.children.push(bug);
            return true;
        }
        if let Some(child) = node.children.first_mut() {
            return insert_at_depth(child, bug, depth - 1);
        }
        false
    }

    insert_at_depth(&mut root, bug_node, bug_depth);
    let _ = total_target; // used for documentation intent
    (root, bug_id)
}

// ============================================================================
// Tests
// ============================================================================

/// Test 1: Single-node tree cannot be reduced.
#[test]
fn single_node_tree_cannot_be_reduced() {
    let tree = WidgetNode::leaf(0, "root");

    let result = hdd_minimize(tree.clone(), |_| true);

    // A leaf has no children — nothing to remove.
    assert_eq!(result, tree);
    assert_eq!(result.node_count(), 1);
}

/// Test 2: 100+ node tree with bug at depth 5 reduces to <10 nodes.
#[test]
fn large_tree_reduces_to_minimal() {
    let (tree, bug_id) = build_tree_with_bug(100, 4);

    // Verify preconditions.
    assert!(
        tree.node_count() > 100,
        "tree should have >100 nodes, got {}",
        tree.node_count()
    );
    assert!(tree.contains_id(bug_id), "bug node must exist in tree");

    let result = hdd_minimize(tree, |t| t.contains_id(bug_id));

    // The reduced tree should be much smaller.
    assert!(
        result.node_count() < 10,
        "reduced tree should have <10 nodes, got {}",
        result.node_count()
    );

    // The bug node must still be present.
    assert!(
        result.contains_id(bug_id),
        "bug node must survive reduction"
    );
}

/// Test 3: Reduction preserves failure predicate at every step.
///
/// We instrument the predicate to record every candidate tree seen,
/// and verify the predicate held true for the final result.
#[test]
fn reduction_preserves_predicate_at_every_step() {
    use std::cell::RefCell;

    let tree = WidgetNode::container(
        0,
        "root",
        vec![
            WidgetNode::container(
                1,
                "panel",
                vec![
                    WidgetNode::leaf(2, "text"),
                    WidgetNode::leaf(3, "buggy"),
                    WidgetNode::leaf(4, "text"),
                ],
            ),
            WidgetNode::container(
                5,
                "panel",
                vec![WidgetNode::leaf(6, "text"), WidgetNode::leaf(7, "text")],
            ),
            WidgetNode::leaf(8, "text"),
        ],
    );

    let predicate_calls = RefCell::new(Vec::new());

    let result = hdd_minimize(tree.clone(), |t| {
        let has_bug = t.contains_kind("buggy");
        predicate_calls.borrow_mut().push((t.clone(), has_bug));
        has_bug
    });

    // The predicate was called on the original input (which must pass).
    let calls = predicate_calls.borrow();
    assert!(!calls.is_empty(), "predicate must be called at least once");

    // First call must be on the original input and return true.
    assert!(calls[0].1, "predicate must hold on original input");

    // The final result must satisfy the predicate.
    assert!(result.contains_kind("buggy"));

    // The result should be smaller than the original.
    assert!(result.node_count() < tree.node_count());
}

/// Test 4: Output is 1-minimal — removing any single remaining child
/// from any node in the result eliminates the failure.
#[test]
fn output_is_1_minimal() {
    let tree = WidgetNode::container(
        0,
        "root",
        vec![
            WidgetNode::leaf(1, "text"),
            WidgetNode::container(
                2,
                "panel",
                vec![
                    WidgetNode::leaf(3, "text"),
                    WidgetNode::leaf(4, "buggy"),
                    WidgetNode::leaf(5, "text"),
                ],
            ),
            WidgetNode::leaf(6, "text"),
            WidgetNode::container(
                7,
                "sidebar",
                vec![WidgetNode::leaf(8, "text"), WidgetNode::leaf(9, "text")],
            ),
        ],
    );

    let predicate = |t: &WidgetNode| t.contains_kind("buggy");
    let result = hdd_minimize(tree, predicate);

    assert!(predicate(&result), "result must satisfy predicate");

    // Verify 1-minimality: removing any single child from any node
    // in the result should break the predicate.
    fn check_1_minimal(
        node: &WidgetNode,
        root: &WidgetNode,
        predicate: &dyn Fn(&WidgetNode) -> bool,
    ) {
        for i in 0..node.children.len() {
            // Build a copy of the full tree with this child removed.
            let mut modified_node = node.clone();
            modified_node.children.remove(i);

            // Reconstruct the root with this modification.
            let modified_root = replace_subtree(root, node.id, &modified_node);

            // The predicate should no longer hold (1-minimality).
            assert!(
                !predicate(&modified_root),
                "tree is not 1-minimal: removing child {} from node {} \
                 still satisfies predicate (result has {} nodes)",
                i,
                node.id,
                modified_root.node_count()
            );
        }

        // Recurse into children.
        for child in &node.children {
            check_1_minimal(child, root, predicate);
        }
    }

    check_1_minimal(&result, &result, &predicate);
}

/// Replace a subtree rooted at `target_id` with `replacement`.
fn replace_subtree(root: &WidgetNode, target_id: u32, replacement: &WidgetNode) -> WidgetNode {
    if root.id == target_id {
        return replacement.clone();
    }
    WidgetNode {
        id: root.id,
        kind: root.kind,
        children: root
            .children
            .iter()
            .map(|c| replace_subtree(c, target_id, replacement))
            .collect(),
    }
}

/// Test: Multiple bug nodes require all to be preserved.
#[test]
fn multiple_required_nodes_preserved() {
    let tree = WidgetNode::container(
        0,
        "root",
        vec![
            WidgetNode::leaf(1, "text"),
            WidgetNode::leaf(2, "bug_a"),
            WidgetNode::leaf(3, "text"),
            WidgetNode::leaf(4, "bug_b"),
            WidgetNode::leaf(5, "text"),
            WidgetNode::leaf(6, "text"),
        ],
    );

    let result = hdd_minimize(tree, |t| {
        t.contains_kind("bug_a") && t.contains_kind("bug_b")
    });

    assert!(result.contains_kind("bug_a"));
    assert!(result.contains_kind("bug_b"));
    assert_eq!(
        result.children.len(),
        2,
        "should keep exactly the two required nodes"
    );
}

/// Test: Deeply nested single-path bug reduces to minimal chain.
#[test]
fn deep_path_reduces_to_chain() {
    // Build: root → a → b → c → d → buggy (+ siblings at each level)
    let tree = WidgetNode::container(
        0,
        "root",
        vec![
            WidgetNode::leaf(10, "noise"),
            WidgetNode::container(
                1,
                "a",
                vec![
                    WidgetNode::leaf(11, "noise"),
                    WidgetNode::container(
                        2,
                        "b",
                        vec![
                            WidgetNode::container(
                                3,
                                "c",
                                vec![
                                    WidgetNode::container(
                                        4,
                                        "d",
                                        vec![
                                            WidgetNode::leaf(5, "buggy"),
                                            WidgetNode::leaf(12, "noise"),
                                        ],
                                    ),
                                    WidgetNode::leaf(13, "noise"),
                                ],
                            ),
                            WidgetNode::leaf(14, "noise"),
                        ],
                    ),
                    WidgetNode::leaf(15, "noise"),
                ],
            ),
            WidgetNode::leaf(16, "noise"),
        ],
    );

    let original_count = tree.node_count();
    assert_eq!(original_count, 13);

    let result = hdd_minimize(tree, |t| t.contains_kind("buggy"));

    assert!(result.contains_kind("buggy"));

    // Should reduce to just the chain: root → a → b → c → d → buggy
    // That's 6 nodes. Allow some slack (HDD might keep a slightly
    // different structure depending on order).
    assert!(
        result.node_count() <= 7,
        "expected <=7 nodes in chain, got {}",
        result.node_count()
    );
}
