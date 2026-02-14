#![forbid(unsafe_code)]

//! E2E integration tests for Hierarchical Delta Debugging.
//!
//! Injects 5 known bug patterns into synthetic widget trees,
//! verifies HDD produces minimal reproducing cases, and logs
//! all reduction steps as structured JSONL.

use std::time::Instant;

use ftui_harness::hdd::{Decomposable, MinimizationResult, ReductionPhase, hdd_minimize_logged};

// ============================================================================
// Widget Tree Model
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
struct Widget {
    id: u32,
    kind: WidgetKind,
    style: StyleInfo,
    text: Option<String>,
    width: u16,
    height: u16,
    children: Vec<Widget>,
}

#[derive(Clone, Debug, PartialEq)]
enum WidgetKind {
    Container,
    Text,
    Button,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct StyleInfo {
    bold: bool,
    fg_set: bool,
    bg_set: bool,
}

impl Widget {
    fn container(id: u32, children: Vec<Widget>) -> Self {
        Self {
            id,
            kind: WidgetKind::Container,
            style: StyleInfo::default(),
            text: None,
            width: 80,
            height: 24,
            children,
        }
    }

    fn text(id: u32, content: &str) -> Self {
        Self {
            id,
            kind: WidgetKind::Text,
            style: StyleInfo::default(),
            text: Some(content.to_string()),
            width: content.len() as u16,
            height: 1,
            children: vec![],
        }
    }

    fn node_count(&self) -> usize {
        1 + self.children.iter().map(|c| c.node_count()).sum::<usize>()
    }

    fn find_by_predicate(&self, pred: &dyn Fn(&Widget) -> bool) -> bool {
        if pred(self) {
            return true;
        }
        self.children.iter().any(|c| c.find_by_predicate(pred))
    }
}

impl Decomposable for Widget {
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
// JSONL Logging
// ============================================================================

fn emit_hdd_result_jsonl(
    bug_id: &str,
    original_size: usize,
    result: &MinimizationResult<Widget>,
    elapsed_ms: u64,
    is_minimal: bool,
) -> String {
    let minimized_size = result.minimized.node_count();
    let reduction_ratio = if original_size > 0 {
        1.0 - (minimized_size as f64 / original_size as f64)
    } else {
        0.0
    };

    format!(
        "{{\"event\":\"hdd_result\",\"bug_id\":\"{bug_id}\",\
         \"original_size\":{original_size},\"minimized_size\":{minimized_size},\
         \"reduction_ratio\":{reduction_ratio:.4},\
         \"total_steps\":{},\"predicate_calls\":{},\
         \"total_time_ms\":{elapsed_ms},\"is_minimal\":{is_minimal}}}",
        result.steps.len(),
        result.predicate_calls,
    )
}

fn emit_step_jsonl(bug_id: &str, step: &ftui_harness::hdd::ReductionStep) -> String {
    format!(
        "{{\"event\":\"hdd_step\",\"bug_id\":\"{bug_id}\",\
         \"step\":{},\"phase\":\"{}\",\
         \"children_before\":{},\"children_after\":{},\
         \"accepted\":{}}}",
        step.step, step.phase, step.children_before, step.children_after, step.accepted,
    )
}

// ============================================================================
// 1-Minimality Check
// ============================================================================

/// Check that removing any single child from any node breaks the predicate.
fn is_1_minimal(root: &Widget, predicate: &dyn Fn(&Widget) -> bool) -> bool {
    check_1_minimal_inner(root, root, predicate)
}

fn check_1_minimal_inner(
    node: &Widget,
    root: &Widget,
    predicate: &dyn Fn(&Widget) -> bool,
) -> bool {
    for i in 0..node.children.len() {
        let mut modified = node.clone();
        modified.children.remove(i);
        let modified_root = replace_subtree(root, node.id, &modified);
        if predicate(&modified_root) {
            return false;
        }
    }
    for child in &node.children {
        if !check_1_minimal_inner(child, root, predicate) {
            return false;
        }
    }
    true
}

fn replace_subtree(root: &Widget, target_id: u32, replacement: &Widget) -> Widget {
    if root.id == target_id {
        return replacement.clone();
    }
    Widget {
        id: root.id,
        kind: root.kind.clone(),
        style: root.style.clone(),
        text: root.text.clone(),
        width: root.width,
        height: root.height,
        children: root
            .children
            .iter()
            .map(|c| replace_subtree(c, target_id, replacement))
            .collect(),
    }
}

// ============================================================================
// Tree Generators
// ============================================================================

/// Generate a large tree with ~100+ nodes containing a specific bug.
fn generate_large_tree(counter: &mut u32, depth: u32, branching: usize) -> Widget {
    let id = *counter;
    *counter += 1;

    if depth == 0 {
        return Widget::text(id, &format!("leaf-{id}"));
    }

    let children = (0..branching)
        .map(|_| generate_large_tree(counter, depth - 1, branching))
        .collect();

    Widget::container(id, children)
}

// ============================================================================
// Bug Definitions
// ============================================================================

/// Bug 1: Off-by-one — a widget has width == 0 (should be >= 1).
fn inject_bug_off_by_one(tree: &mut Widget, target_id: u32) {
    if tree.id == target_id {
        tree.width = 0;
        return;
    }
    for child in &mut tree.children {
        inject_bug_off_by_one(child, target_id);
    }
}

fn has_bug_off_by_one(w: &Widget) -> bool {
    w.find_by_predicate(&|w| w.width == 0)
}

/// Bug 2: Missing style — a button lacks bold styling.
fn inject_bug_missing_style(tree: &mut Widget, target_id: u32) {
    if tree.id == target_id {
        tree.kind = WidgetKind::Button;
        tree.text = Some("Submit".to_string());
        tree.style.bold = false; // Bug: button should be bold
        return;
    }
    for child in &mut tree.children {
        inject_bug_missing_style(child, target_id);
    }
}

fn has_bug_missing_style(w: &Widget) -> bool {
    w.find_by_predicate(&|w| matches!(w.kind, WidgetKind::Button) && !w.style.bold)
}

/// Bug 3: Wrong layout — a container with marker width=7 has children
/// totaling > parent width. The marker width is unique and no generated
/// node has it, ensuring the predicate only fires on the injected bug.
fn inject_bug_wrong_layout(tree: &mut Widget, target_id: u32) {
    if tree.id == target_id {
        tree.kind = WidgetKind::Container;
        tree.width = 7; // Unique marker width
        tree.style.fg_set = true; // Extra marker
        tree.children = vec![Widget::text(tree.id + 1000, "very long text content here")];
        return;
    }
    for child in &mut tree.children {
        inject_bug_wrong_layout(child, target_id);
    }
}

fn has_bug_wrong_layout(w: &Widget) -> bool {
    w.find_by_predicate(&|w| {
        // Only match the specific injected bug via marker width + fg_set.
        if !matches!(w.kind, WidgetKind::Container) || w.width != 7 || !w.style.fg_set {
            return false;
        }
        let children_width: u16 = w.children.iter().map(|c| c.width).sum();
        children_width > w.width
    })
}

/// Bug 4: Null text — a text widget has None text content.
fn inject_bug_null_text(tree: &mut Widget, target_id: u32) {
    if tree.id == target_id {
        tree.kind = WidgetKind::Text;
        tree.text = None; // Bug: text widget must have text
        return;
    }
    for child in &mut tree.children {
        inject_bug_null_text(child, target_id);
    }
}

fn has_bug_null_text(w: &Widget) -> bool {
    w.find_by_predicate(&|w| matches!(w.kind, WidgetKind::Text) && w.text.is_none())
}

/// Bug 5: Overflow — a widget dimension exceeds u16::MAX / 2.
fn inject_bug_overflow(tree: &mut Widget, target_id: u32) {
    if tree.id == target_id {
        tree.width = 40000;
        tree.height = 40000;
        return;
    }
    for child in &mut tree.children {
        inject_bug_overflow(child, target_id);
    }
}

fn has_bug_overflow(w: &Widget) -> bool {
    w.find_by_predicate(&|w| w.width > 32767 || w.height > 32767)
}

// ============================================================================
// E2E Tests
// ============================================================================

struct BugSpec {
    id: &'static str,
    inject: fn(&mut Widget, u32),
    detect: fn(&Widget) -> bool,
    bug_depth: u32,
}

const BUG_SPECS: &[BugSpec] = &[
    BugSpec {
        id: "off_by_one",
        inject: inject_bug_off_by_one,
        detect: has_bug_off_by_one,
        bug_depth: 3,
    },
    BugSpec {
        id: "missing_style",
        inject: inject_bug_missing_style,
        detect: has_bug_missing_style,
        bug_depth: 2,
    },
    BugSpec {
        id: "wrong_layout",
        inject: inject_bug_wrong_layout,
        detect: has_bug_wrong_layout,
        bug_depth: 4,
    },
    BugSpec {
        id: "null_text",
        inject: inject_bug_null_text,
        detect: has_bug_null_text,
        bug_depth: 3,
    },
    BugSpec {
        id: "overflow",
        inject: inject_bug_overflow,
        detect: has_bug_overflow,
        bug_depth: 4,
    },
];

fn find_node_at_depth(tree: &Widget, depth: u32) -> Option<u32> {
    if depth == 0 {
        return Some(tree.id);
    }
    for child in &tree.children {
        if let Some(id) = find_node_at_depth(child, depth - 1) {
            return Some(id);
        }
    }
    None
}

/// Run a single bug scenario through HDD and return structured results.
fn run_bug_scenario(spec: &BugSpec) -> (MinimizationResult<Widget>, usize, u64, bool) {
    let mut counter = 0u32;
    // branching=3, depth=4 → 1+3+9+27+81 = 121 nodes
    let mut tree = generate_large_tree(&mut counter, 4, 3);
    let original_size = tree.node_count();

    // Find a node at the target depth and inject the bug.
    let target_id =
        find_node_at_depth(&tree, spec.bug_depth).expect("tree must have node at bug depth");
    (spec.inject)(&mut tree, target_id);

    assert!(
        (spec.detect)(&tree),
        "bug {} must be detectable after injection",
        spec.id
    );

    let start = Instant::now();
    let result = hdd_minimize_logged(tree, spec.detect);
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let is_minimal = is_1_minimal(&result.minimized, &|w| (spec.detect)(w));

    (result, original_size, elapsed_ms, is_minimal)
}

#[test]
fn e2e_bug_off_by_one() {
    let spec = &BUG_SPECS[0];
    let (result, original_size, elapsed_ms, is_minimal) = run_bug_scenario(spec);

    // Verify bug preserved.
    assert!(
        has_bug_off_by_one(&result.minimized),
        "bug must survive reduction"
    );

    // Verify reduction.
    let minimized_size = result.minimized.node_count();
    let ratio = 1.0 - (minimized_size as f64 / original_size as f64);
    assert!(ratio >= 0.80, "reduction ratio {ratio:.2} < 0.80");

    // Verify 1-minimality.
    assert!(is_minimal, "result must be 1-minimal");

    // Emit JSONL for audit.
    let jsonl = emit_hdd_result_jsonl(spec.id, original_size, &result, elapsed_ms, is_minimal);
    eprintln!("{jsonl}");

    assert!(elapsed_ms < 60_000, "must complete in <60s");
}

#[test]
fn e2e_bug_missing_style() {
    let spec = &BUG_SPECS[1];
    let (result, original_size, elapsed_ms, is_minimal) = run_bug_scenario(spec);

    assert!(has_bug_missing_style(&result.minimized));
    let minimized_size = result.minimized.node_count();
    let ratio = 1.0 - (minimized_size as f64 / original_size as f64);
    assert!(ratio >= 0.80, "reduction ratio {ratio:.2} < 0.80");
    assert!(is_minimal, "result must be 1-minimal");

    let jsonl = emit_hdd_result_jsonl(spec.id, original_size, &result, elapsed_ms, is_minimal);
    eprintln!("{jsonl}");
    assert!(elapsed_ms < 60_000);
}

#[test]
fn e2e_bug_wrong_layout() {
    let spec = &BUG_SPECS[2];
    let (result, original_size, elapsed_ms, is_minimal) = run_bug_scenario(spec);

    assert!(has_bug_wrong_layout(&result.minimized));
    let minimized_size = result.minimized.node_count();
    let ratio = 1.0 - (minimized_size as f64 / original_size as f64);
    assert!(ratio >= 0.80, "reduction ratio {ratio:.2} < 0.80");
    assert!(is_minimal, "result must be 1-minimal");

    let jsonl = emit_hdd_result_jsonl(spec.id, original_size, &result, elapsed_ms, is_minimal);
    eprintln!("{jsonl}");
    assert!(elapsed_ms < 60_000);
}

#[test]
fn e2e_bug_null_text() {
    let spec = &BUG_SPECS[3];
    let (result, original_size, elapsed_ms, is_minimal) = run_bug_scenario(spec);

    assert!(has_bug_null_text(&result.minimized));
    let minimized_size = result.minimized.node_count();
    let ratio = 1.0 - (minimized_size as f64 / original_size as f64);
    assert!(ratio >= 0.80, "reduction ratio {ratio:.2} < 0.80");
    assert!(is_minimal, "result must be 1-minimal");

    let jsonl = emit_hdd_result_jsonl(spec.id, original_size, &result, elapsed_ms, is_minimal);
    eprintln!("{jsonl}");
    assert!(elapsed_ms < 60_000);
}

#[test]
fn e2e_bug_overflow() {
    let spec = &BUG_SPECS[4];
    let (result, original_size, elapsed_ms, is_minimal) = run_bug_scenario(spec);

    assert!(has_bug_overflow(&result.minimized));
    let minimized_size = result.minimized.node_count();
    let ratio = 1.0 - (minimized_size as f64 / original_size as f64);
    assert!(ratio >= 0.80, "reduction ratio {ratio:.2} < 0.80");
    assert!(is_minimal, "result must be 1-minimal");

    let jsonl = emit_hdd_result_jsonl(spec.id, original_size, &result, elapsed_ms, is_minimal);
    eprintln!("{jsonl}");
    assert!(elapsed_ms < 60_000);
}

/// Validate all 5 bugs produce valid JSONL step logs.
#[test]
fn e2e_all_bugs_produce_valid_jsonl() {
    for spec in BUG_SPECS {
        let (result, _original_size, _elapsed_ms, _is_minimal) = run_bug_scenario(spec);

        // Verify step JSONL is parseable.
        for step in &result.steps {
            let line = emit_step_jsonl(spec.id, step);
            let parsed: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("invalid JSONL for {}: {e}", spec.id));
            assert_eq!(parsed["event"], "hdd_step");
            assert_eq!(parsed["bug_id"], spec.id);
        }

        // Verify result JSONL is parseable.
        let result_line = emit_hdd_result_jsonl(
            spec.id, 122, // approximate original
            &result, 0, true,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result_line)
            .unwrap_or_else(|e| panic!("invalid result JSONL for {}: {e}", spec.id));
        assert_eq!(parsed["event"], "hdd_result");
    }
}

/// Verify that HDD logs contain both accepted and rejected steps.
#[test]
fn e2e_logs_contain_both_outcomes() {
    let spec = &BUG_SPECS[0]; // off_by_one with 121 nodes
    let (result, _original_size, _elapsed_ms, _is_minimal) = run_bug_scenario(spec);

    let accepted = result.steps.iter().filter(|s| s.accepted).count();
    let rejected = result.steps.iter().filter(|s| !s.accepted).count();

    assert!(accepted > 0, "must have accepted steps");
    assert!(
        rejected > 0,
        "must have rejected steps (not all attempts succeed)"
    );

    // Verify phase diversity.
    let has_chunk_removal = result
        .steps
        .iter()
        .any(|s| s.phase == ReductionPhase::ChunkRemoval);
    assert!(has_chunk_removal, "should have chunk removal steps");
}
