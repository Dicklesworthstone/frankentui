#![forbid(unsafe_code)]

//! Persistent-snapshot undo/redo store.
//!
//! [`SnapshotStore`] provides O(1) snapshot capture and restore using
//! [`Arc`]-based structural sharing. Each call to [`push`](SnapshotStore::push)
//! clones the `Arc` (not the state), so 1000 snapshots of a large state
//! cost barely more than a single copy when the state uses persistent
//! data structures (e.g., `im::HashMap`, `im::Vector`).
//!
//! # Architecture
//!
//! ```text
//! push(s3)
//! ┌──────────────────────────────────────────────────┐
//! │ Undo Stack:  [Arc(s0), Arc(s1), Arc(s2), Arc(s3)]│
//! │ Redo Stack:  []                                   │
//! │ Current:     Arc(s3)                              │
//! └──────────────────────────────────────────────────┘
//!
//! undo() x2
//! ┌──────────────────────────────────────────────────┐
//! │ Undo Stack:  [Arc(s0), Arc(s1)]                  │
//! │ Redo Stack:  [Arc(s2), Arc(s3)]                  │
//! │ Current:     Arc(s1)                              │
//! └──────────────────────────────────────────────────┘
//!
//! push(s4) — new branch, clears redo
//! ┌──────────────────────────────────────────────────┐
//! │ Undo Stack:  [Arc(s0), Arc(s1), Arc(s4)]         │
//! │ Redo Stack:  []                                   │
//! │ Current:     Arc(s4)                              │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! # When to Use
//!
//! Use `SnapshotStore` when your state type `T` uses persistent collections
//! (e.g., `im::HashMap`, `im::Vector`) so that `Arc::new(state.clone())`
//! is cheap thanks to structural sharing. For command-pattern undo
//! (reversible mutations), use [`HistoryManager`](super::HistoryManager).
//!
//! # Memory Model
//!
//! Each snapshot is an `Arc<T>`. When `T` uses persistent data structures,
//! cloning `T` shares most of the underlying memory. The store enforces
//! configurable depth limits. Memory is reclaimed when the last `Arc`
//! referencing a snapshot is dropped.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

/// Configuration for the snapshot store.
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// Maximum number of snapshots to retain in the undo stack.
    /// Oldest snapshots are evicted when this limit is exceeded.
    pub max_depth: usize,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self { max_depth: 100 }
    }
}

impl SnapshotConfig {
    /// Create a new configuration with the given depth limit.
    #[must_use]
    pub fn new(max_depth: usize) -> Self {
        Self { max_depth }
    }

    /// Create an unlimited configuration (for testing).
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            max_depth: usize::MAX,
        }
    }
}

/// A snapshot-based undo/redo store using `Arc<T>` for structural sharing.
///
/// `T` should ideally use persistent data structures internally (e.g.,
/// `im::HashMap`) so that cloning is O(1) and snapshots share memory.
///
/// # Invariants
///
/// 1. `undo_stack` is never empty after the first `push`.
/// 2. `undo_stack.len() <= config.max_depth` (after any operation).
/// 3. Redo stack is cleared on every `push`.
/// 4. `current()` always returns the most recently pushed or restored snapshot.
pub struct SnapshotStore<T> {
    /// Snapshots available for undo (current state is at the back).
    undo_stack: VecDeque<Arc<T>>,
    /// Snapshots available for redo (most recently undone at back).
    redo_stack: VecDeque<Arc<T>>,
    /// Configuration.
    config: SnapshotConfig,
}

impl<T: fmt::Debug> fmt::Debug for SnapshotStore<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotStore")
            .field("undo_depth", &self.undo_stack.len())
            .field("redo_depth", &self.redo_stack.len())
            .field("config", &self.config)
            .finish()
    }
}

impl<T> SnapshotStore<T> {
    /// Create a new snapshot store with the given configuration.
    #[must_use]
    pub fn new(config: SnapshotConfig) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            config,
        }
    }

    /// Create a new snapshot store with default configuration.
    #[must_use]
    pub fn with_default_config() -> Self {
        Self::new(SnapshotConfig::default())
    }

    // ====================================================================
    // Core Operations
    // ====================================================================

    /// Push a new snapshot, clearing the redo stack (new branch).
    ///
    /// The snapshot is wrapped in `Arc` for structural sharing.
    /// If the undo stack exceeds `max_depth`, the oldest snapshot is evicted.
    pub fn push(&mut self, state: T) {
        self.redo_stack.clear();
        self.undo_stack.push_back(Arc::new(state));
        self.enforce_depth();
    }

    /// Push a pre-wrapped `Arc<T>` snapshot.
    ///
    /// Use this when you already have an `Arc<T>` and want to avoid
    /// double-wrapping.
    pub fn push_arc(&mut self, state: Arc<T>) {
        self.redo_stack.clear();
        self.undo_stack.push_back(state);
        self.enforce_depth();
    }

    /// Undo: move the current snapshot to the redo stack and return
    /// the previous snapshot.
    ///
    /// Returns `None` if there is only one snapshot (the initial state)
    /// or the store is empty.
    pub fn undo(&mut self) -> Option<Arc<T>> {
        // Need at least 2 items: the one we pop goes to redo, the new back is current
        if self.undo_stack.len() < 2 {
            return None;
        }
        let current = self.undo_stack.pop_back()?;
        self.redo_stack.push_back(current);
        self.undo_stack.back().cloned()
    }

    /// Redo: move the most recently undone snapshot back to the undo stack.
    ///
    /// Returns `None` if there is nothing to redo.
    pub fn redo(&mut self) -> Option<Arc<T>> {
        let snapshot = self.redo_stack.pop_back()?;
        self.undo_stack.push_back(snapshot);
        self.undo_stack.back().cloned()
    }

    /// Get the current snapshot (the most recent on the undo stack).
    ///
    /// Returns `None` if the store is empty.
    #[must_use]
    pub fn current(&self) -> Option<&Arc<T>> {
        self.undo_stack.back()
    }

    // ====================================================================
    // Query
    // ====================================================================

    /// Check if undo is available.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.undo_stack.len() >= 2
    }

    /// Check if redo is available.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Number of snapshots on the undo stack (including current).
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Number of snapshots on the redo stack.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Total number of snapshots across both stacks.
    #[must_use]
    pub fn total_snapshots(&self) -> usize {
        self.undo_stack.len() + self.redo_stack.len()
    }

    /// Get the configuration.
    #[must_use]
    pub fn config(&self) -> &SnapshotConfig {
        &self.config
    }

    /// Check if the store is empty (no snapshots at all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.undo_stack.is_empty()
    }

    // ====================================================================
    // Maintenance
    // ====================================================================

    /// Clear all snapshots.
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Enforce the depth limit by evicting the oldest snapshots.
    fn enforce_depth(&mut self) {
        while self.undo_stack.len() > self.config.max_depth {
            self.undo_stack.pop_front();
        }
    }
}

// ============================================================================
// Re-export persistent data structure types when the `hamt` feature is enabled
// ============================================================================

/// Persistent collection types for snapshot-friendly state.
///
/// When the `hamt` feature is enabled, this module re-exports types from
/// the [`im`] crate. These collections use hash-array-mapped tries (HAMT)
/// and relaxed-radix-balanced trees (RRB) for O(log n) structural sharing
/// on clone.
///
/// # Example
///
/// ```ignore
/// use ftui_runtime::undo::snapshot_store::persistent;
///
/// let mut map = persistent::HashMap::new();
/// map.insert("key", 42);
/// let snapshot = map.clone(); // O(log n) — shares structure
/// map.insert("key2", 99);
/// // `snapshot` still has only "key" → 42
/// ```
#[cfg(feature = "hamt")]
pub mod persistent {
    pub use im::{HashMap, HashSet, OrdMap, OrdSet, Vector};
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_is_empty() {
        let store = SnapshotStore::<i32>::with_default_config();
        assert!(store.is_empty());
        assert!(!store.can_undo());
        assert!(!store.can_redo());
        assert_eq!(store.undo_depth(), 0);
        assert_eq!(store.redo_depth(), 0);
        assert_eq!(store.total_snapshots(), 0);
        assert!(store.current().is_none());
    }

    #[test]
    fn push_makes_current_available() {
        let mut store = SnapshotStore::with_default_config();
        store.push(42);
        assert!(!store.is_empty());
        assert_eq!(**store.current().unwrap(), 42);
        assert_eq!(store.undo_depth(), 1);
        assert!(!store.can_undo()); // Need at least 2 to undo
    }

    #[test]
    fn push_two_enables_undo() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        assert!(store.can_undo());
        assert!(!store.can_redo());
        assert_eq!(store.undo_depth(), 2);
    }

    #[test]
    fn undo_restores_previous() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.push(3);

        let prev = store.undo().unwrap();
        assert_eq!(*prev, 2);
        assert_eq!(**store.current().unwrap(), 2);
        assert!(store.can_redo());
    }

    #[test]
    fn undo_all_stops_at_initial() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.push(3);

        assert!(store.undo().is_some()); // 3 → 2
        assert!(store.undo().is_some()); // 2 → 1
        assert!(store.undo().is_none()); // can't undo past initial
        assert_eq!(**store.current().unwrap(), 1);
    }

    #[test]
    fn redo_restores_undone() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.undo();

        let restored = store.redo().unwrap();
        assert_eq!(*restored, 2);
        assert_eq!(**store.current().unwrap(), 2);
        assert!(!store.can_redo());
    }

    #[test]
    fn push_clears_redo() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.undo();
        assert!(store.can_redo());

        store.push(3); // New branch
        assert!(!store.can_redo());
        assert_eq!(store.redo_depth(), 0);
        assert_eq!(**store.current().unwrap(), 3);
    }

    #[test]
    fn redo_on_empty_returns_none() {
        let mut store = SnapshotStore::<i32>::with_default_config();
        assert!(store.redo().is_none());
    }

    #[test]
    fn undo_on_empty_returns_none() {
        let mut store = SnapshotStore::<i32>::with_default_config();
        assert!(store.undo().is_none());
    }

    #[test]
    fn undo_on_single_returns_none() {
        let mut store = SnapshotStore::with_default_config();
        store.push(42);
        assert!(store.undo().is_none());
    }

    #[test]
    fn depth_limit_evicts_oldest() {
        let mut store = SnapshotStore::new(SnapshotConfig::new(3));
        store.push(1);
        store.push(2);
        store.push(3);
        store.push(4);

        assert_eq!(store.undo_depth(), 3);
        // Oldest (1) was evicted. Current is 4, can undo to 3 then 2.
        assert_eq!(**store.current().unwrap(), 4);
        let prev = store.undo().unwrap();
        assert_eq!(*prev, 3);
        let prev = store.undo().unwrap();
        assert_eq!(*prev, 2);
        assert!(store.undo().is_none());
    }

    #[test]
    fn depth_limit_one_keeps_only_latest() {
        let mut store = SnapshotStore::new(SnapshotConfig::new(1));
        store.push(1);
        store.push(2);
        store.push(3);

        assert_eq!(store.undo_depth(), 1);
        assert_eq!(**store.current().unwrap(), 3);
        assert!(!store.can_undo());
    }

    #[test]
    fn clear_removes_all() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.undo();

        store.clear();

        assert!(store.is_empty());
        assert!(!store.can_undo());
        assert!(!store.can_redo());
        assert_eq!(store.total_snapshots(), 0);
    }

    #[test]
    fn multiple_undo_redo_cycle() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.push(3);

        // Undo all
        store.undo(); // → 2
        store.undo(); // → 1

        assert_eq!(store.undo_depth(), 1);
        assert_eq!(store.redo_depth(), 2);

        // Redo all
        store.redo(); // → 2
        store.redo(); // → 3

        assert_eq!(store.undo_depth(), 3);
        assert_eq!(store.redo_depth(), 0);
        assert_eq!(**store.current().unwrap(), 3);
    }

    #[test]
    fn push_arc_avoids_double_wrap() {
        let mut store = SnapshotStore::with_default_config();
        let arc = Arc::new(42);
        store.push_arc(arc.clone());

        assert_eq!(**store.current().unwrap(), 42);
        // The Arc in the store should be the same as our original
        assert!(Arc::ptr_eq(store.current().unwrap(), &arc));
    }

    #[test]
    fn structural_sharing_verified() {
        // Verify that multiple snapshots share the same Arc
        let mut store = SnapshotStore::with_default_config();
        let state = Arc::new(vec![1, 2, 3, 4, 5]);
        store.push_arc(state.clone());

        // Push same Arc again (simulating clone of persistent state)
        store.push_arc(state.clone());

        // Both snapshots should point to the same allocation
        let s1 = store.undo().unwrap();
        let s2 = store.current().unwrap();
        // After undo, s1 is the undone (2nd push), s2 is current (1st push)
        // They should both be the same Arc
        assert!(Arc::ptr_eq(&s1, s2));
    }

    #[test]
    fn many_snapshots_within_memory() {
        // Verify that 1000 snapshots of an Arc<Vec> don't blow up memory
        let mut store = SnapshotStore::new(SnapshotConfig::new(1000));
        let data = Arc::new(vec![0u8; 1024]); // 1KB payload

        for _ in 0..1000 {
            store.push_arc(data.clone());
        }

        // All 1000 snapshots share the same underlying Vec
        assert_eq!(store.undo_depth(), 1000);
        assert_eq!(Arc::strong_count(&data), 1001); // 1 original + 1000 in store
    }

    #[test]
    fn config_default() {
        let config = SnapshotConfig::default();
        assert_eq!(config.max_depth, 100);
    }

    #[test]
    fn config_unlimited() {
        let config = SnapshotConfig::unlimited();
        assert_eq!(config.max_depth, usize::MAX);
    }

    #[test]
    fn config_clone() {
        let config = SnapshotConfig::new(50);
        let cloned = config.clone();
        assert_eq!(cloned.max_depth, 50);
    }

    #[test]
    fn config_debug() {
        let config = SnapshotConfig::new(42);
        let s = format!("{config:?}");
        assert!(s.contains("42"));
    }

    #[test]
    fn store_debug() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        let s = format!("{store:?}");
        assert!(s.contains("SnapshotStore"));
        assert!(s.contains("undo_depth"));
    }

    #[test]
    fn config_accessor() {
        let store = SnapshotStore::<i32>::new(SnapshotConfig::new(42));
        assert_eq!(store.config().max_depth, 42);
    }

    #[test]
    fn total_snapshots_accounts_for_both_stacks() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.push(3);
        assert_eq!(store.total_snapshots(), 3);

        store.undo();
        assert_eq!(store.total_snapshots(), 3); // 2 undo + 1 redo

        store.undo();
        assert_eq!(store.total_snapshots(), 3); // 1 undo + 2 redo
    }

    // ====================================================================
    // im crate integration tests (always available via dev-dependency)
    // ====================================================================

    #[test]
    fn im_hashmap_structural_sharing() {
        use im::HashMap;

        let mut map = HashMap::new();
        for i in 0..1000 {
            map.insert(format!("key_{i}"), i);
        }

        let mut store = SnapshotStore::with_default_config();

        // Push initial state
        store.push(map.clone());

        // Mutate and push — clone is O(log n) due to structural sharing
        let mut map2 = map.clone();
        map2.insert("new_key".to_string(), 9999);
        store.push(map2);

        // Undo should restore the original map
        let prev = store.undo().unwrap();
        assert_eq!(prev.len(), 1000);
        assert!(!prev.contains_key("new_key"));

        // Redo should restore the mutated map
        let restored = store.redo().unwrap();
        assert_eq!(restored.len(), 1001);
        assert_eq!(restored.get("new_key"), Some(&9999));
    }

    #[test]
    fn im_vector_structural_sharing() {
        use im::Vector;

        let mut vec: Vector<u32> = (0..1000).collect();

        let mut store = SnapshotStore::with_default_config();
        store.push(vec.clone());

        // Mutate (append)
        vec.push_back(9999);
        store.push(vec);

        // Undo
        let prev = store.undo().unwrap();
        assert_eq!(prev.len(), 1000);

        // Redo
        let restored = store.redo().unwrap();
        assert_eq!(restored.len(), 1001);
        assert_eq!(restored.back(), Some(&9999));
    }

    #[test]
    fn im_hashmap_many_snapshots_memory_efficiency() {
        use im::HashMap;

        // Create a "large" state
        let mut state: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..100 {
            state.insert(format!("entry_{i}"), vec![0u8; 100]);
        }

        let mut store = SnapshotStore::new(SnapshotConfig::new(1000));

        // Take 50 snapshots with small mutations each
        for i in 0..50 {
            store.push(state.clone());
            // Small mutation — only 1 key changes
            state.insert(format!("entry_{}", i % 100), vec![i as u8; 100]);
        }

        assert_eq!(store.undo_depth(), 50);

        // All snapshots should be valid and distinct
        for _ in 0..49 {
            let prev = store.undo().unwrap();
            assert_eq!(prev.len(), 100);
        }
        assert!(store.undo().is_none());
    }

    #[test]
    fn depth_limit_zero_evicts_everything() {
        let mut store = SnapshotStore::new(SnapshotConfig::new(0));
        store.push(42);
        assert!(store.is_empty());
    }

    #[test]
    fn push_arc_clears_redo() {
        let mut store = SnapshotStore::with_default_config();
        store.push(1);
        store.push(2);
        store.undo();
        assert!(store.can_redo());

        store.push_arc(Arc::new(3));
        assert!(!store.can_redo());
    }

    #[test]
    fn undo_redo_returns_correct_arc() {
        let mut store = SnapshotStore::with_default_config();
        store.push("a");
        store.push("b");
        store.push("c");

        let undone = store.undo().unwrap();
        assert_eq!(*undone, "b");

        let redone = store.redo().unwrap();
        assert_eq!(*redone, "c");
    }
}
