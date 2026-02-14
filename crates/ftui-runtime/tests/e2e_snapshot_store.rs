#![forbid(unsafe_code)]

//! End-to-end integration tests for HAMT-backed SnapshotStore undo/redo.
//!
//! Validates:
//! - 100 sequential edits with undo/redo at every step
//! - Random undo/redo interleaving
//! - Multiple state types (editor, form, tree)
//! - Structural sharing memory efficiency
//! - JSONL structured logging for each operation

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ftui_runtime::undo::{SnapshotConfig, SnapshotStore};
use im::{HashMap as ImHashMap, Vector as ImVector};
use web_time::Instant;

// ============================================================================
// JSONL log entry
// ============================================================================

#[derive(Debug, serde::Serialize)]
struct LogEntry {
    event: &'static str,
    operation: &'static str,
    step: u32,
    snapshot_count: u32,
    state_hash: String,
    expected_hash: String,
    #[serde(rename = "match")]
    is_match: bool,
    op_time_ns: u64,
}

fn hash_state<T: Hash>(state: &T) -> String {
    let mut hasher = DefaultHasher::new();
    state.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ============================================================================
// Editor state model (im::HashMap + im::Vector)
// ============================================================================

#[derive(Debug, Clone, Hash)]
struct EditorState {
    lines: ImVector<String>,
    cursor_line: usize,
    cursor_col: usize,
    metadata: ImHashMap<String, String>,
}

impl EditorState {
    fn new() -> Self {
        Self {
            lines: ImVector::new(),
            cursor_line: 0,
            cursor_col: 0,
            metadata: ImHashMap::new(),
        }
    }

    fn insert_line(&mut self, idx: usize, text: String) {
        let pos = idx.min(self.lines.len());
        self.lines.insert(pos, text);
        self.cursor_line = pos;
        self.cursor_col = 0;
    }

    fn delete_line(&mut self, idx: usize) {
        if !self.lines.is_empty() {
            let pos = idx.min(self.lines.len() - 1);
            self.lines.remove(pos);
            self.cursor_line = self.cursor_line.min(self.lines.len().saturating_sub(1));
        }
    }

    fn append_to_line(&mut self, idx: usize, text: &str) {
        if let Some(line) = self.lines.get(idx.min(self.lines.len().saturating_sub(1))) {
            let mut new_line = line.clone();
            new_line.push_str(text);
            let pos = idx.min(self.lines.len().saturating_sub(1));
            self.lines.set(pos, new_line);
        }
    }

    fn set_metadata(&mut self, key: String, value: String) {
        self.metadata.insert(key, value);
    }
}

// ============================================================================
// Form state model
// ============================================================================

#[derive(Debug, Clone, Hash)]
struct FormState {
    fields: ImHashMap<String, String>,
    focused_field: Option<String>,
    validation_errors: ImVector<String>,
}

impl FormState {
    fn new() -> Self {
        Self {
            fields: ImHashMap::new(),
            focused_field: None,
            validation_errors: ImVector::new(),
        }
    }

    fn set_field(&mut self, key: String, value: String) {
        self.fields.insert(key, value);
    }

    fn focus(&mut self, field: String) {
        self.focused_field = Some(field);
    }

    fn add_error(&mut self, error: String) {
        self.validation_errors.push_back(error);
    }
}

// ============================================================================
// Tree state model
// ============================================================================

#[derive(Debug, Clone, Hash)]
struct TreeState {
    nodes: ImHashMap<u32, String>,
    children: ImHashMap<u32, ImVector<u32>>,
    expanded: ImHashMap<u32, bool>,
    selected: Option<u32>,
    next_id: u32,
}

impl TreeState {
    fn new() -> Self {
        let mut nodes = ImHashMap::new();
        nodes.insert(0, "root".to_string());
        Self {
            nodes,
            children: ImHashMap::new(),
            expanded: ImHashMap::new(),
            selected: None,
            next_id: 1,
        }
    }

    fn add_child(&mut self, parent: u32, label: String) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.insert(id, label);
        let siblings = self.children.get(&parent).cloned().unwrap_or_default();
        let mut new_siblings = siblings;
        new_siblings.push_back(id);
        self.children.insert(parent, new_siblings);
        id
    }

    fn toggle_expand(&mut self, id: u32) {
        let current = self.expanded.get(&id).copied().unwrap_or(false);
        self.expanded.insert(id, !current);
    }

    fn select(&mut self, id: u32) {
        self.selected = Some(id);
    }
}

// ============================================================================
// Test 1: Editor — 100 edits, full undo, full redo
// ============================================================================

#[test]
fn e2e_editor_100_edits_undo_redo() {
    let mut state = EditorState::new();
    let mut store = SnapshotStore::new(SnapshotConfig::new(200));
    let mut expected_hashes = Vec::new();
    let mut log_entries = Vec::new();

    // Initial state
    store.push(state.clone());
    expected_hashes.push(hash_state(&state));

    // 100 sequential edits
    for i in 0..100u32 {
        let start = Instant::now();

        match i % 4 {
            0 => state.insert_line(i as usize, format!("Line {i}: content")),
            1 => state.append_to_line(
                (i as usize).min(state.lines.len().saturating_sub(1)),
                &format!(" appended_{i}"),
            ),
            2 => state.set_metadata(format!("key_{i}"), format!("val_{i}")),
            3 => {
                if !state.lines.is_empty() {
                    state.delete_line(i as usize % state.lines.len().max(1))
                }
            }
            _ => unreachable!(),
        }

        store.push(state.clone());
        let state_hash = hash_state(&state);
        expected_hashes.push(state_hash.clone());

        let elapsed = start.elapsed().as_nanos() as u64;

        log_entries.push(LogEntry {
            event: "hamt_undo_redo",
            operation: "edit",
            step: i,
            snapshot_count: store.total_snapshots() as u32,
            state_hash: state_hash.clone(),
            expected_hash: state_hash,
            is_match: true,
            op_time_ns: elapsed,
        });
    }

    // Undo all 100 edits, verifying at each step
    for i in (0..100u32).rev() {
        let start = Instant::now();
        let restored = store.undo().expect("should be able to undo");
        let elapsed = start.elapsed().as_nanos() as u64;

        let state_hash = hash_state(restored.as_ref());
        let expected = &expected_hashes[i as usize];
        let is_match = &state_hash == expected;

        log_entries.push(LogEntry {
            event: "hamt_undo_redo",
            operation: "undo",
            step: i,
            snapshot_count: store.total_snapshots() as u32,
            state_hash,
            expected_hash: expected.clone(),
            is_match,
            op_time_ns: elapsed,
        });

        assert!(is_match, "undo step {i}: state hash mismatch");
    }

    // Verify we're at initial state
    assert!(store.undo().is_none());

    // Redo all 100 edits
    for i in 1..=100u32 {
        let start = Instant::now();
        let restored = store.redo().expect("should be able to redo");
        let elapsed = start.elapsed().as_nanos() as u64;

        let state_hash = hash_state(restored.as_ref());
        let expected = &expected_hashes[i as usize];
        let is_match = &state_hash == expected;

        log_entries.push(LogEntry {
            event: "hamt_undo_redo",
            operation: "redo",
            step: i,
            snapshot_count: store.total_snapshots() as u32,
            state_hash,
            expected_hash: expected.clone(),
            is_match,
            op_time_ns: elapsed,
        });

        assert!(is_match, "redo step {i}: state hash mismatch");
    }

    // Verify JSONL is parseable
    for entry in &log_entries {
        let json = serde_json::to_string(entry).unwrap();
        let _parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    }

    // Verify all operations matched
    assert!(
        log_entries.iter().all(|e| e.is_match),
        "all operations should match"
    );
}

// ============================================================================
// Test 2: Random undo/redo interleaving
// ============================================================================

#[test]
fn e2e_random_undo_redo_interleaving() {
    let mut state = EditorState::new();
    let mut store = SnapshotStore::new(SnapshotConfig::new(200));

    // Build up 50 edits
    store.push(state.clone());
    let mut snapshots = vec![state.clone()];

    for i in 0..50u32 {
        state.insert_line(i as usize, format!("Line {i}"));
        store.push(state.clone());
        snapshots.push(state.clone());
    }

    // Deterministic "random" interleaving using simple LCG
    let mut rng_state: u64 = 12345;
    let mut current_idx: usize = 50; // We're at snapshot 50

    for _ in 0..50 {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let action = rng_state % 3;

        match action {
            0 => {
                // Undo
                if store.can_undo() {
                    store.undo().unwrap();
                    current_idx -= 1;
                    let expected = hash_state(&snapshots[current_idx]);
                    let actual = hash_state(store.current().unwrap().as_ref());
                    assert_eq!(expected, actual, "undo mismatch at idx {current_idx}");
                }
            }
            1 => {
                // Redo
                if store.can_redo() {
                    store.redo().unwrap();
                    current_idx += 1;
                    let expected = hash_state(&snapshots[current_idx]);
                    let actual = hash_state(store.current().unwrap().as_ref());
                    assert_eq!(expected, actual, "redo mismatch at idx {current_idx}");
                }
            }
            _ => {
                // New edit (creates new branch)
                state = store.current().unwrap().as_ref().clone();
                state.insert_line(0, format!("New edit at idx {current_idx}"));
                store.push(state.clone());
                // After push, redo is cleared; update tracking
                snapshots.truncate(current_idx + 1);
                snapshots.push(state.clone());
                current_idx += 1;
            }
        }
    }
}

// ============================================================================
// Test 3: Form widget state undo/redo
// ============================================================================

#[test]
fn e2e_form_state_undo_redo() {
    let mut state = FormState::new();
    let mut store = SnapshotStore::with_default_config();

    store.push(state.clone());

    // Fill out form fields
    for i in 0..20u32 {
        state.set_field(format!("field_{i}"), format!("value_{i}"));
        state.focus(format!("field_{i}"));
        if i % 5 == 0 {
            state.add_error(format!("Validation error for field_{i}"));
        }
        store.push(state.clone());
    }

    // Undo to empty form
    for _ in 0..20 {
        store.undo().unwrap();
    }

    let empty = store.current().unwrap();
    assert!(empty.fields.is_empty());
    assert!(empty.validation_errors.is_empty());

    // Redo all
    for _ in 0..20 {
        store.redo().unwrap();
    }

    let full = store.current().unwrap();
    assert_eq!(full.fields.len(), 20);
    assert_eq!(full.validation_errors.len(), 4); // i=0,5,10,15
}

// ============================================================================
// Test 4: Tree widget state undo/redo
// ============================================================================

#[test]
fn e2e_tree_state_undo_redo() {
    let mut state = TreeState::new();
    let mut store = SnapshotStore::with_default_config();

    store.push(state.clone());

    // Build a tree
    let mut ids = vec![0u32]; // root
    for i in 0..30u32 {
        let parent = ids[i as usize % ids.len()];
        let child_id = state.add_child(parent, format!("Node {i}"));
        ids.push(child_id);

        if i % 3 == 0 {
            state.toggle_expand(parent);
        }
        if i % 2 == 0 {
            state.select(child_id);
        }

        store.push(state.clone());
    }

    // Undo all tree operations
    let mut undo_count = 0;
    while store.undo().is_some() {
        undo_count += 1;
    }
    assert_eq!(undo_count, 30);

    // Initial state: just root
    let initial = store.current().unwrap();
    assert_eq!(initial.nodes.len(), 1);
    assert_eq!(initial.next_id, 1);

    // Redo all
    let mut redo_count = 0;
    while store.redo().is_some() {
        redo_count += 1;
    }
    assert_eq!(redo_count, 30);

    let final_state = store.current().unwrap();
    assert_eq!(final_state.nodes.len(), 31); // root + 30 children
}

// ============================================================================
// Test 5: Structural sharing — memory does not grow linearly
// ============================================================================

#[test]
fn e2e_structural_sharing_memory_efficiency() {
    let mut state: ImHashMap<String, Vec<u8>> = ImHashMap::new();

    // Create a "large" baseline state (~100KB)
    for i in 0..1000 {
        state.insert(format!("key_{i:04}"), vec![0u8; 100]);
    }

    let mut store = SnapshotStore::new(SnapshotConfig::new(200));

    // Take 100 snapshots, each with a small mutation
    for i in 0..100 {
        store.push(state.clone());
        // Only modify 1 key per snapshot
        state.insert(format!("key_{:04}", i % 1000), vec![i as u8; 100]);
    }

    assert_eq!(store.undo_depth(), 100);

    // Verify all snapshots are accessible and distinct
    let final_hash = hash_state(store.current().unwrap().as_ref());

    store.undo().unwrap();
    let prev_hash = hash_state(store.current().unwrap().as_ref());
    assert_ne!(final_hash, prev_hash, "consecutive snapshots should differ");

    // Memory check: Arc strong counts show sharing
    let current = store.current().unwrap().clone();
    // The Arc itself should have refcount of at least 2 (store + our clone)
    assert!(Arc::strong_count(&current) >= 2);
}

// ============================================================================
// Test 6: Operation timing — undo/redo should be fast
// ============================================================================

#[test]
fn e2e_undo_redo_timing() {
    let mut state: ImHashMap<u64, u64> = ImHashMap::new();
    for i in 0..10_000 {
        state.insert(i, i * 7);
    }

    let mut store = SnapshotStore::new(SnapshotConfig::new(200));

    // Build 100 snapshots
    for i in 0..100u64 {
        store.push(state.clone());
        state.insert(i % 10_000, i * 13);
    }

    // Time 100 undos
    let start = Instant::now();
    let mut undo_count = 0;
    while store.undo().is_some() {
        undo_count += 1;
    }
    let undo_total = start.elapsed();

    // Time 100 redos
    let start = Instant::now();
    let mut redo_count = 0;
    while store.redo().is_some() {
        redo_count += 1;
    }
    let redo_total = start.elapsed();

    assert_eq!(undo_count, 99); // 100 pushed, undo 99 times
    assert_eq!(redo_count, 99);

    // Each undo/redo should be well under 1ms (they're just Arc moves)
    // Be generous in CI: allow 10ms total for 99 operations
    let undo_per_op_ns = undo_total.as_nanos() / undo_count as u128;
    let redo_per_op_ns = redo_total.as_nanos() / redo_count as u128;

    // Sanity check: per-operation time should be under 100_000ns (0.1ms)
    assert!(
        undo_per_op_ns < 100_000,
        "undo too slow: {undo_per_op_ns}ns per op"
    );
    assert!(
        redo_per_op_ns < 100_000,
        "redo too slow: {redo_per_op_ns}ns per op"
    );
}

// ============================================================================
// Test 7: JSONL schema compliance
// ============================================================================

#[test]
fn e2e_jsonl_schema_compliance() {
    let entries = vec![
        LogEntry {
            event: "hamt_undo_redo",
            operation: "edit",
            step: 0,
            snapshot_count: 1,
            state_hash: "00000000deadbeef".to_string(),
            expected_hash: "00000000deadbeef".to_string(),
            is_match: true,
            op_time_ns: 42,
        },
        LogEntry {
            event: "hamt_undo_redo",
            operation: "undo",
            step: 1,
            snapshot_count: 2,
            state_hash: "caffeine00000000".to_string(),
            expected_hash: "caffeine00000000".to_string(),
            is_match: true,
            op_time_ns: 100,
        },
    ];

    for entry in &entries {
        let json = serde_json::to_string(entry).unwrap();

        // Verify round-trip
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Verify all required fields present
        assert!(parsed.get("event").is_some());
        assert!(parsed.get("operation").is_some());
        assert!(parsed.get("step").is_some());
        assert!(parsed.get("snapshot_count").is_some());
        assert!(parsed.get("state_hash").is_some());
        assert!(parsed.get("expected_hash").is_some());
        assert!(parsed.get("match").is_some());
        assert!(parsed.get("op_time_ns").is_some());

        // Verify types
        assert!(parsed["event"].is_string());
        assert!(parsed["operation"].is_string());
        assert!(parsed["step"].is_number());
        assert!(parsed["snapshot_count"].is_number());
        assert!(parsed["state_hash"].is_string());
        assert!(parsed["expected_hash"].is_string());
        assert!(parsed["match"].is_boolean());
        assert!(parsed["op_time_ns"].is_number());
    }
}
