#![forbid(unsafe_code)]

//! Hierarchical Delta Debugging (HDD) for structured inputs.
//!
//! Recursively removes parts of structured input while a failure predicate
//! still holds, producing a minimal failing test case. Applies to widget
//! trees, event sequences, state configurations, or any [`Decomposable`]
//! structure.
//!
//! # Algorithm
//!
//! 1. Try removing children in halves (binary search reduction).
//! 2. If removing a half still triggers the predicate, keep that reduction.
//! 3. Otherwise split into smaller groups and retry.
//! 4. When no more children can be removed at the current level, recurse
//!    into each remaining child.
//!
//! # Example
//!
//! ```rust
//! use ftui_harness::hdd::{Decomposable, hdd_minimize};
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct Tree {
//!     label: &'static str,
//!     children: Vec<Tree>,
//! }
//!
//! impl Decomposable for Tree {
//!     fn children(&self) -> Vec<Self> {
//!         self.children.clone()
//!     }
//!     fn remove_child(&mut self, idx: usize) {
//!         self.children.remove(idx);
//!     }
//!     fn replace_children(&mut self, new_children: Vec<Self>) {
//!         self.children = new_children;
//!     }
//! }
//!
//! let tree = Tree {
//!     label: "root",
//!     children: vec![
//!         Tree { label: "a", children: vec![] },
//!         Tree { label: "b", children: vec![] },  // ← causes failure
//!         Tree { label: "c", children: vec![] },
//!     ],
//! };
//!
//! // Predicate: fails when tree contains a child labeled "b"
//! let minimal = hdd_minimize(tree, |t| {
//!     t.children.iter().any(|c| c.label == "b")
//! });
//!
//! assert_eq!(minimal.children.len(), 1);
//! assert_eq!(minimal.children[0].label, "b");
//! ```

/// A structure that can be decomposed into children for delta debugging.
///
/// Implementors expose their child elements so the HDD algorithm can
/// try removing subsets to find a minimal failing configuration.
pub trait Decomposable: Clone {
    /// Return a snapshot of the current children.
    fn children(&self) -> Vec<Self>;

    /// Remove the child at position `idx`.
    ///
    /// # Panics
    ///
    /// May panic if `idx` is out of bounds.
    fn remove_child(&mut self, idx: usize);

    /// Replace all children with `new_children`.
    fn replace_children(&mut self, new_children: Vec<Self>);
}

/// Minimize a structured input using Hierarchical Delta Debugging.
///
/// Repeatedly removes subsets of children at each level of the tree
/// while `predicate` still returns `true` (i.e., the failure still
/// reproduces). Returns the smallest structure that still satisfies
/// the predicate.
///
/// The predicate should return `true` when the failure is present
/// (the "interesting" condition holds).
pub fn hdd_minimize<T, F>(input: T, predicate: F) -> T
where
    T: Decomposable,
    F: Fn(&T) -> bool,
{
    assert!(
        predicate(&input),
        "predicate must hold on the original input"
    );
    hdd_minimize_inner(input, &predicate)
}

fn hdd_minimize_inner<T, F>(mut input: T, predicate: &F) -> T
where
    T: Decomposable,
    F: Fn(&T) -> bool,
{
    // Phase 1: minimize the set of children at this level using ddmin.
    input = ddmin_children(input, predicate);

    // Phase 2: recurse into each remaining child.
    let mut children = input.children();
    for i in 0..children.len() {
        let minimized = hdd_minimize_inner(children[i].clone(), predicate);

        // Try replacing this child with its minimized version.
        let original = children[i].clone();
        children[i] = minimized;

        let mut candidate = input.clone();
        candidate.replace_children(children.clone());

        if predicate(&candidate) {
            input = candidate;
        } else {
            // Minimized child broke the predicate; restore original.
            children[i] = original;
        }
    }

    input
}

/// Delta-debugging minimization of children at a single level.
///
/// Implements the ddmin algorithm: try removing halves, then quarters,
/// etc. of the children list while the predicate holds.
fn ddmin_children<T, F>(mut input: T, predicate: &F) -> T
where
    T: Decomposable,
    F: Fn(&T) -> bool,
{
    let mut n = 2usize;

    loop {
        let children = input.children();
        let len = children.len();

        if len == 0 {
            break;
        }

        let chunk_size = len.div_ceil(n);
        let mut reduced = false;

        // Try removing each chunk.
        let mut i = 0;
        while i < n {
            let start = i * chunk_size;
            let end = (start + chunk_size).min(len);
            if start >= len {
                break;
            }

            // Build candidate with chunk [start..end) removed.
            let mut candidate = input.clone();
            let remaining: Vec<T> = children
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx < start || *idx >= end)
                .map(|(_, c)| c.clone())
                .collect();
            candidate.replace_children(remaining);

            if predicate(&candidate) {
                input = candidate;
                n = 2;
                reduced = true;
                break;
            }
            i += 1;
        }

        if reduced {
            continue;
        }

        // Try keeping each chunk (complement removal).
        // Only useful when there are at least 2 chunks.
        if n > 1 {
            let mut i = 0;
            while i < n {
                let start = i * chunk_size;
                let end = (start + chunk_size).min(len);
                if start >= len {
                    break;
                }

                let kept_len = end - start;
                // Skip if keeping this chunk doesn't reduce the size.
                if kept_len >= len {
                    i += 1;
                    continue;
                }

                // Build candidate keeping only chunk [start..end).
                let mut candidate = input.clone();
                let kept: Vec<T> = children[start..end].to_vec();
                candidate.replace_children(kept);

                if predicate(&candidate) {
                    input = candidate;
                    n = 2;
                    reduced = true;
                    break;
                }
                i += 1;
            }
        }

        if reduced {
            continue;
        }

        // Increase granularity.
        if n >= len {
            break;
        }
        n = (n * 2).min(len);
    }

    input
}

// =========================================================================
// Logged Minimization (proptest post-shrink integration)
// =========================================================================

use std::cell::RefCell;
use std::fmt;

/// A single reduction step recorded during logged minimization.
#[derive(Clone, Debug)]
pub struct ReductionStep {
    /// Step number (0-indexed).
    pub step: usize,
    /// What phase produced this step.
    pub phase: ReductionPhase,
    /// Number of children before this step.
    pub children_before: usize,
    /// Number of children after this step.
    pub children_after: usize,
    /// Whether the predicate held (and the reduction was accepted).
    pub accepted: bool,
}

/// The phase of HDD that produced a reduction step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReductionPhase {
    /// Removing a chunk of children (ddmin).
    ChunkRemoval,
    /// Keeping only a subset of children (ddmin complement).
    ChunkRetention,
    /// Recursive minimization of a child subtree.
    ChildRecursion,
}

impl fmt::Display for ReductionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChunkRemoval => write!(f, "chunk_removal"),
            Self::ChunkRetention => write!(f, "chunk_retention"),
            Self::ChildRecursion => write!(f, "child_recursion"),
        }
    }
}

/// Result of a logged HDD minimization.
#[derive(Clone, Debug)]
pub struct MinimizationResult<T> {
    /// The minimized input.
    pub minimized: T,
    /// Log of all reduction steps attempted.
    pub steps: Vec<ReductionStep>,
    /// Total predicate evaluations.
    pub predicate_calls: usize,
}

impl<T> MinimizationResult<T> {
    /// Serialize the reduction log as JSONL (one JSON object per line).
    pub fn steps_to_jsonl(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!(
                "{{\"step\":{},\"phase\":\"{}\",\"children_before\":{},\"children_after\":{},\"accepted\":{}}}",
                step.step, step.phase, step.children_before, step.children_after, step.accepted
            ));
        }
        out
    }
}

/// Minimize a structured input using HDD, logging every reduction step.
///
/// This is the proptest post-shrink integration point. After proptest finds
/// a counterexample and performs its own shrinking, pass the result to this
/// function for further structural minimization with a full audit trail.
///
/// Returns a [`MinimizationResult`] containing the minimized input, a log
/// of all reduction steps, and the total number of predicate evaluations.
pub fn hdd_minimize_logged<T, F>(input: T, predicate: F) -> MinimizationResult<T>
where
    T: Decomposable,
    F: Fn(&T) -> bool,
{
    let log = RefCell::new(Vec::new());
    let call_count = RefCell::new(0usize);

    let logging_predicate = |t: &T| -> bool {
        *call_count.borrow_mut() += 1;
        predicate(t)
    };

    assert!(
        logging_predicate(&input),
        "predicate must hold on the original input"
    );

    let minimized = hdd_logged_inner(input, &logging_predicate, &log);

    MinimizationResult {
        minimized,
        steps: log.into_inner(),
        predicate_calls: call_count.into_inner(),
    }
}

fn hdd_logged_inner<T, F>(mut input: T, predicate: &F, log: &RefCell<Vec<ReductionStep>>) -> T
where
    T: Decomposable,
    F: Fn(&T) -> bool,
{
    // Phase 1: ddmin with logging.
    input = ddmin_children_logged(input, predicate, log);

    // Phase 2: recurse into each remaining child.
    let mut children = input.children();
    for i in 0..children.len() {
        let before_count = count_children_recursive(&children[i]);
        let minimized = hdd_logged_inner(children[i].clone(), predicate, log);
        let after_count = count_children_recursive(&minimized);

        let original = children[i].clone();
        children[i] = minimized;

        let mut candidate = input.clone();
        candidate.replace_children(children.clone());

        let accepted = predicate(&candidate);

        let step_num = log.borrow().len();
        log.borrow_mut().push(ReductionStep {
            step: step_num,
            phase: ReductionPhase::ChildRecursion,
            children_before: before_count,
            children_after: if accepted { after_count } else { before_count },
            accepted,
        });

        if accepted {
            input = candidate;
        } else {
            children[i] = original;
        }
    }

    input
}

fn ddmin_children_logged<T, F>(mut input: T, predicate: &F, log: &RefCell<Vec<ReductionStep>>) -> T
where
    T: Decomposable,
    F: Fn(&T) -> bool,
{
    let mut n = 2usize;

    loop {
        let children = input.children();
        let len = children.len();

        if len == 0 {
            break;
        }

        let chunk_size = len.div_ceil(n);
        let mut reduced = false;

        // Try removing each chunk.
        let mut i = 0;
        while i < n {
            let start = i * chunk_size;
            let end = (start + chunk_size).min(len);
            if start >= len {
                break;
            }

            let mut candidate = input.clone();
            let remaining: Vec<T> = children
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx < start || *idx >= end)
                .map(|(_, c)| c.clone())
                .collect();
            let new_len = remaining.len();
            candidate.replace_children(remaining);

            let accepted = predicate(&candidate);

            let step_num = log.borrow().len();
            log.borrow_mut().push(ReductionStep {
                step: step_num,
                phase: ReductionPhase::ChunkRemoval,
                children_before: len,
                children_after: if accepted { new_len } else { len },
                accepted,
            });

            if accepted {
                input = candidate;
                n = 2;
                reduced = true;
                break;
            }
            i += 1;
        }

        if reduced {
            continue;
        }

        // Try keeping each chunk.
        if n > 1 {
            let mut i = 0;
            while i < n {
                let start = i * chunk_size;
                let end = (start + chunk_size).min(len);
                if start >= len {
                    break;
                }

                let kept_len = end - start;
                if kept_len >= len {
                    i += 1;
                    continue;
                }

                let mut candidate = input.clone();
                let kept: Vec<T> = children[start..end].to_vec();
                candidate.replace_children(kept);

                let accepted = predicate(&candidate);

                let step_num = log.borrow().len();
                log.borrow_mut().push(ReductionStep {
                    step: step_num,
                    phase: ReductionPhase::ChunkRetention,
                    children_before: len,
                    children_after: if accepted { kept_len } else { len },
                    accepted,
                });

                if accepted {
                    input = candidate;
                    n = 2;
                    reduced = true;
                    break;
                }
                i += 1;
            }
        }

        if reduced {
            continue;
        }

        if n >= len {
            break;
        }
        n = (n * 2).min(len);
    }

    input
}

/// Count total children recursively (used for step logging).
fn count_children_recursive<T: Decomposable>(node: &T) -> usize {
    let children = node.children();
    children.len() + children.iter().map(count_children_recursive).sum::<usize>()
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Tree {
        label: String,
        children: Vec<Tree>,
    }

    impl Tree {
        fn leaf(label: &str) -> Self {
            Self {
                label: label.to_string(),
                children: vec![],
            }
        }

        fn node(label: &str, children: Vec<Tree>) -> Self {
            Self {
                label: label.to_string(),
                children,
            }
        }
    }

    impl Decomposable for Tree {
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

    // Vec<T> is also decomposable (for event sequences).
    // A Vec with 0 or 1 elements is a leaf (no further decomposition).
    impl<T: Clone> Decomposable for Vec<T> {
        fn children(&self) -> Vec<Self> {
            if self.len() <= 1 {
                return vec![];
            }
            self.iter().map(|item| vec![item.clone()]).collect()
        }

        fn remove_child(&mut self, idx: usize) {
            self.remove(idx);
        }

        fn replace_children(&mut self, new_children: Vec<Self>) {
            *self = new_children.into_iter().flatten().collect();
        }
    }

    #[test]
    fn single_child_preserved() {
        let tree = Tree::node("root", vec![Tree::leaf("only")]);
        let result = hdd_minimize(tree, |t| t.children.iter().any(|c| c.label == "only"));
        assert_eq!(result.children.len(), 1);
        assert_eq!(result.children[0].label, "only");
    }

    #[test]
    fn removes_irrelevant_children() {
        let tree = Tree::node(
            "root",
            vec![
                Tree::leaf("a"),
                Tree::leaf("b"),
                Tree::leaf("trigger"),
                Tree::leaf("c"),
                Tree::leaf("d"),
            ],
        );

        let result = hdd_minimize(tree, |t| t.children.iter().any(|c| c.label == "trigger"));

        assert_eq!(result.children.len(), 1);
        assert_eq!(result.children[0].label, "trigger");
    }

    #[test]
    fn preserves_two_required_children() {
        let tree = Tree::node(
            "root",
            vec![
                Tree::leaf("a"),
                Tree::leaf("needed1"),
                Tree::leaf("b"),
                Tree::leaf("needed2"),
                Tree::leaf("c"),
            ],
        );

        let result = hdd_minimize(tree, |t| {
            let labels: Vec<&str> = t.children.iter().map(|c| c.label.as_str()).collect();
            labels.contains(&"needed1") && labels.contains(&"needed2")
        });

        assert_eq!(result.children.len(), 2);
        let labels: Vec<&str> = result.children.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"needed1"));
        assert!(labels.contains(&"needed2"));
    }

    #[test]
    fn recurses_into_children() {
        let tree = Tree::node(
            "root",
            vec![Tree::node(
                "parent",
                vec![Tree::leaf("x"), Tree::leaf("culprit"), Tree::leaf("y")],
            )],
        );

        let result = hdd_minimize(tree, |t| {
            fn has_culprit(t: &Tree) -> bool {
                if t.label == "culprit" {
                    return true;
                }
                t.children.iter().any(has_culprit)
            }
            has_culprit(t)
        });

        // Root should have 1 child ("parent"), which has 1 child ("culprit").
        assert_eq!(result.children.len(), 1);
        assert_eq!(result.children[0].label, "parent");
        assert_eq!(result.children[0].children.len(), 1);
        assert_eq!(result.children[0].children[0].label, "culprit");
    }

    #[test]
    fn empty_children_is_fixpoint() {
        let tree = Tree::leaf("root");
        let result = hdd_minimize(tree.clone(), |_| true);
        assert_eq!(result, tree);
    }

    #[test]
    fn event_sequence_minimization() {
        let events: Vec<i32> = vec![1, 2, 3, 4, 5, 6, 7, 8];

        // Predicate: sequence must contain 3 and 7.
        let result = hdd_minimize(events, |seq| seq.contains(&3) && seq.contains(&7));

        assert!(result.contains(&3));
        assert!(result.contains(&7));
        assert!(result.len() <= 3); // Should be close to minimal.
    }

    #[test]
    fn deep_nested_minimization() {
        let tree = Tree::node(
            "root",
            vec![
                Tree::node(
                    "branch1",
                    vec![
                        Tree::leaf("noise1"),
                        Tree::node(
                            "sub",
                            vec![Tree::leaf("noise2"), Tree::leaf("deep_trigger")],
                        ),
                    ],
                ),
                Tree::leaf("noise3"),
            ],
        );

        let result = hdd_minimize(tree, |t| {
            fn find_label(t: &Tree, label: &str) -> bool {
                if t.label == label {
                    return true;
                }
                t.children.iter().any(|c| find_label(c, label))
            }
            find_label(t, "deep_trigger")
        });

        // Should drill down to the minimal path.
        fn find_label(t: &Tree, label: &str) -> bool {
            if t.label == label {
                return true;
            }
            t.children.iter().any(|c| find_label(c, label))
        }
        assert!(find_label(&result, "deep_trigger"));

        // Count total nodes — should be much less than original 7.
        fn count_nodes(t: &Tree) -> usize {
            1 + t.children.iter().map(count_nodes).sum::<usize>()
        }
        assert!(count_nodes(&result) <= 4);
    }

    #[test]
    #[should_panic(expected = "predicate must hold")]
    fn panics_if_predicate_fails_on_input() {
        let tree = Tree::leaf("root");
        hdd_minimize(tree, |_| false);
    }

    #[test]
    fn all_children_needed() {
        let tree = Tree::node(
            "root",
            vec![Tree::leaf("a"), Tree::leaf("b"), Tree::leaf("c")],
        );

        let result = hdd_minimize(tree.clone(), |t| t.children.len() == 3);

        assert_eq!(result.children.len(), 3);
    }

    #[test]
    fn large_input_binary_search_efficiency() {
        // With 100 children, ddmin should not need O(n) predicate calls.
        let children: Vec<Tree> = (0..100).map(|i| Tree::leaf(&format!("n{i}"))).collect();
        let tree = Tree::node("root", children);

        let call_count = std::cell::Cell::new(0u64);
        let result = hdd_minimize(tree, |t| {
            call_count.set(call_count.get() + 1);
            t.children.iter().any(|c| c.label == "n42")
        });

        assert_eq!(result.children.len(), 1);
        assert_eq!(result.children[0].label, "n42");

        // ddmin on n=100 with 1 target should need O(log n) calls, not O(n).
        // Generous bound: should be well under 100.
        assert!(
            call_count.get() < 50,
            "too many predicate calls: {}",
            call_count.get()
        );
    }

    // =====================================================================
    // Logged minimization tests
    // =====================================================================

    #[test]
    fn logged_minimization_produces_steps() {
        let tree = Tree::node(
            "root",
            vec![
                Tree::leaf("a"),
                Tree::leaf("trigger"),
                Tree::leaf("b"),
                Tree::leaf("c"),
            ],
        );

        let result = hdd_minimize_logged(tree, |t| t.children.iter().any(|c| c.label == "trigger"));

        assert_eq!(result.minimized.children.len(), 1);
        assert_eq!(result.minimized.children[0].label, "trigger");
        assert!(!result.steps.is_empty(), "should have logged steps");
        assert!(result.predicate_calls > 0);
    }

    #[test]
    fn logged_steps_contain_accepted_reductions() {
        let tree = Tree::node(
            "root",
            vec![
                Tree::leaf("a"),
                Tree::leaf("b"),
                Tree::leaf("trigger"),
                Tree::leaf("c"),
                Tree::leaf("d"),
            ],
        );

        let result = hdd_minimize_logged(tree, |t| t.children.iter().any(|c| c.label == "trigger"));

        // At least one step should have been accepted.
        let accepted_count = result.steps.iter().filter(|s| s.accepted).count();
        assert!(
            accepted_count > 0,
            "at least one reduction must be accepted"
        );

        // Accepted steps should show actual reduction.
        for step in result.steps.iter().filter(|s| s.accepted) {
            assert!(
                step.children_after <= step.children_before,
                "accepted step should not increase children"
            );
        }
    }

    #[test]
    fn jsonl_output_is_valid() {
        let tree = Tree::node(
            "root",
            vec![Tree::leaf("a"), Tree::leaf("trigger"), Tree::leaf("b")],
        );

        let result = hdd_minimize_logged(tree, |t| t.children.iter().any(|c| c.label == "trigger"));

        let jsonl = result.steps_to_jsonl();
        assert!(!jsonl.is_empty());

        // Each line should be valid JSON.
        for line in jsonl.lines() {
            let parsed: serde_json::Value =
                serde_json::from_str(line).expect("each JSONL line must be valid JSON");
            assert!(parsed.get("step").is_some());
            assert!(parsed.get("phase").is_some());
            assert!(parsed.get("accepted").is_some());
        }
    }

    #[test]
    fn logged_predicate_count_matches() {
        let tree = Tree::node(
            "root",
            vec![Tree::leaf("a"), Tree::leaf("trigger"), Tree::leaf("b")],
        );

        let manual_count = std::cell::Cell::new(0u64);
        let result = hdd_minimize_logged(tree, |t| {
            manual_count.set(manual_count.get() + 1);
            t.children.iter().any(|c| c.label == "trigger")
        });

        // The logged predicate_calls should match our manual count.
        assert_eq!(result.predicate_calls, manual_count.get() as usize);
    }
}
