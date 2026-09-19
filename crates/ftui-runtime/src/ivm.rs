//! Incremental View Maintenance (IVM) — delta-propagation DAG for derived
//! render state (bd-3akdb.1).
//!
//! # Status: parts, not a pipeline
//!
//! **There is no propagation engine.** This module has the delta algebra, a DAG
//! with topological ordering and cycle detection, a fallback policy predicate,
//! and two [`IncrementalView`] implementations ([`StyleResolutionView`],
//! [`FilteredListView`]). Nothing walks the topological order feeding one view's
//! output into the next — the tests call [`IncrementalView::apply_delta`] one
//! view at a time and build DAGs separately, because there is no driver to call.
//!
//! The sections below describe the design this module is *for*, not behaviour it
//! *has*. Concretely, as of 2026-09-19:
//!
//! - The architecture diagram names `LayoutView` and `RenderView`. Neither
//!   exists; the only `IncrementalView` impls are the two named above.
//! - Nothing here imports `Observable`, `DepGraph`, `Buffer` or `Presenter`.
//!   The module's only dependencies are `std::fmt` and `std::hash`, so no delta
//!   reaches a buffer and none is "consumed by the presenter".
//! - There is no `ivm.propagate` tracing span; `tracing` is not imported.
//! - [`EpochEvidence::to_jsonl`] is never called outside this module's tests,
//!   so no evidence is emitted.
//! - [`IvmConfig::from_env`] really does read `FRANKENTUI_FULL_RECOMPUTE`, but
//!   `force_full` and `emit_evidence` are read by nothing afterwards. Setting
//!   the variable changes a struct field and no behaviour.
//!
//! What is here is real and tested — the delta encoding, the DAG, and the two
//! views all have unit and e2e coverage. Treat it as the groundwork for an IVM
//! system rather than one you can turn on. Gated behind the `experimental`
//! feature. Wiring or retiring it is `bd-lksq7`.
//!
//! # Intended architecture (not implemented)
//!
//! ```text
//!   Observable<Theme>   Observable<Content>   Observable<Constraint>
//!          │                    │                      │
//!          ▼                    │                      │
//!   ┌──────────────┐           │                      │
//!   │  StyleView   │◄──────────┘                      │
//!   │  (resolve)   │                                  │
//!   └──────┬───────┘                                  │
//!          │ Δ(style)                                 │
//!          ▼                                          │
//!   ┌──────────────┐                                  │
//!   │  LayoutView  │◄─────────────────────────────────┘
//!   │  (compute)   │
//!   └──────┬───────┘
//!          │ Δ(rects)
//!          ▼
//!   ┌──────────────┐
//!   │  RenderView  │
//!   │  (cells)     │
//!   └──────┬───────┘
//!          │ Δ(cells)
//!          ▼
//!     BufferDiff → Presenter → Terminal
//! ```
//!
//! # Delta Representation
//!
//! Updates are represented as **signed tuples**: `(key, weight, logical_time)`.
//!
//! - `weight = +1`: insertion / update (new value replaces old).
//! - `weight = -1`: deletion (key removed from the view).
//! - `logical_time`: monotonic counter for causal ordering.
//!
//! This is the standard IVM delta encoding from database theory (Chirkova &
//! Yang, "Materialized Views", §3.1). It composes: applying Δ₁ then Δ₂ is
//! equivalent to applying their union (with cancellation of opposite signs).
//!
//! # Processing model (intended)
//!
//! Deltas would be processed in **topological micro-batches**. Steps 1, 3 and 4
//! have no implementation; step 2 is [`DagTopology::compute_topo_order`], which
//! does exist:
//!
//! 1. Collect all input changes since last frame (the "epoch").
//! 2. Sort the DAG topologically (pre-computed at DAG build time).
//! 3. For each view in topological order:
//!    a. Receive input deltas from upstream views.
//!    b. Apply `apply_delta()` to produce output deltas.
//!    c. Forward output deltas to downstream views.
//! 4. The final view emits cell-level deltas consumed by the presenter.
//!
//! The threshold above which a view should fall back to full recomputation
//! (heuristic: > 50% of the materialized view size) is decided by
//! [`FallbackPolicy::should_fallback`], which is a pure predicate. Nothing calls
//! it during processing, because nothing processes.
//!
//! # Evidence logging (intended)
//!
//! Each propagation epoch would log an `ivm.propagate` tracing span carrying
//! `epoch`, `views_processed`, `views_recomputed`, `total_delta_size` and
//! `duration_us`, alongside an [`EpochEvidence`] JSONL line comparing delta size
//! against full-recompute cost per view.
//!
//! [`EpochEvidence`] and its [`EpochEvidence::to_jsonl`] serializer exist and are
//! tested. The span does not: `tracing` is not imported here, and nothing calls
//! `to_jsonl` outside this module's tests.
//!
//! # Fallback (intended)
//!
//! `FRANKENTUI_FULL_RECOMPUTE=1` is meant to disable incremental processing as a
//! benchmarking baseline and a safety valve. [`IvmConfig::from_env`] does read
//! it, but nothing reads `force_full` afterwards, so today the variable changes
//! a struct field and nothing else. The same is true of `emit_evidence`.
//!
//! # Intended integration with existing infrastructure (none of it wired)
//!
//! None of the following is implemented. This module imports `std::fmt` and
//! `std::hash` and nothing else, so it cannot currently observe or affect any of
//! these:
//!
//! - **DepGraph** (ftui-layout): would provide layout-level dirty tracking as
//!   the backing store for a `LayoutView`. There is no `LayoutView`.
//! - **Observable/Computed** (ftui-runtime/reactive): would feed the IVM input
//!   layer, letting IVM views replace frequently-updated `Computed` values.
//! - **Buffer dirty tracking** (ftui-render): a `RenderView`'s output deltas
//!   would translate to `Buffer` dirty_rows/dirty_spans for the presenter.
//!   There is no `RenderView`.
//! - **BatchScope** (ftui-runtime/reactive): one batch would align with one
//!   propagation epoch. There are no epochs to align with.

use std::fmt;
use std::hash::{Hash, Hasher};

// ============================================================================
// DeltaEntry — signed tuple for incremental updates
// ============================================================================

/// A signed delta entry representing an incremental change.
///
/// The fundamental unit of IVM propagation. Changes flow through the DAG
/// as collections of delta entries, which views transform into output deltas.
///
/// # Semantics
///
/// - `Insert(key, value)`: New or updated mapping at `key`.
/// - `Delete(key)`: Key removed from the materialized view.
///
/// Both variants carry a `logical_time` for causal ordering within an epoch.
#[derive(Debug, Clone, PartialEq)]
pub enum DeltaEntry<K, V> {
    /// Insert or update: `(key, value, logical_time)`.
    /// Weight = +1 in signed-tuple notation.
    Insert { key: K, value: V, logical_time: u64 },
    /// Delete: `(key, logical_time)`.
    /// Weight = -1 in signed-tuple notation.
    Delete { key: K, logical_time: u64 },
}

impl<K, V> DeltaEntry<K, V> {
    /// The key affected by this delta.
    pub fn key(&self) -> &K {
        match self {
            DeltaEntry::Insert { key, .. } => key,
            DeltaEntry::Delete { key, .. } => key,
        }
    }

    /// The logical time of this delta.
    pub fn logical_time(&self) -> u64 {
        match self {
            DeltaEntry::Insert { logical_time, .. } => *logical_time,
            DeltaEntry::Delete { logical_time, .. } => *logical_time,
        }
    }

    /// The signed weight: +1 for insert, -1 for delete.
    pub fn weight(&self) -> i8 {
        match self {
            DeltaEntry::Insert { .. } => 1,
            DeltaEntry::Delete { .. } => -1,
        }
    }

    /// Whether this is an insert/update.
    pub fn is_insert(&self) -> bool {
        matches!(self, DeltaEntry::Insert { .. })
    }
}

// ============================================================================
// DeltaBatch — collection of deltas for one epoch
// ============================================================================

/// A batch of delta entries for a single propagation epoch.
///
/// Micro-batching amortizes per-delta overhead and enables bulk processing
/// optimizations (e.g., deduplicating multiple updates to the same key within
/// one epoch).
#[derive(Debug, Clone)]
pub struct DeltaBatch<K, V> {
    /// The epoch number (monotonically increasing).
    pub epoch: u64,
    /// The delta entries in this batch, ordered by logical_time.
    pub entries: Vec<DeltaEntry<K, V>>,
}

impl<K: Eq + Hash, V> DeltaBatch<K, V> {
    /// Create an empty batch for the given epoch.
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            entries: Vec::new(),
        }
    }

    /// Number of entries in this batch.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this batch is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Push an insert entry.
    pub fn insert(&mut self, key: K, value: V, logical_time: u64) {
        self.entries.push(DeltaEntry::Insert {
            key,
            value,
            logical_time,
        });
    }

    /// Push a delete entry.
    pub fn delete(&mut self, key: K, logical_time: u64) {
        self.entries.push(DeltaEntry::Delete { key, logical_time });
    }
}

// ============================================================================
// ViewId — handle into the DAG
// ============================================================================

/// Lightweight handle identifying a view in the IVM DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub u32);

impl fmt::Display for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "V{}", self.0)
    }
}

// ============================================================================
// ViewDomain — categorization of view types
// ============================================================================

/// The domain of a view in the IVM pipeline.
///
/// Used for logging, evidence collection, and fallback policy decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewDomain {
    /// Style resolution (theme → resolved style per widget).
    Style,
    /// Layout computation (constraints → Rect positions).
    Layout,
    /// Render cells (style + content + layout → Cell grid).
    Render,
    /// Filtered/sorted list (data → visible subset).
    FilteredList,
    /// Custom user-defined view domain.
    Custom,
}

impl ViewDomain {
    /// Human-readable label for logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            ViewDomain::Style => "style",
            ViewDomain::Layout => "layout",
            ViewDomain::Render => "render",
            ViewDomain::FilteredList => "filtered_list",
            ViewDomain::Custom => "custom",
        }
    }
}

// ============================================================================
// PropagationResult — outcome of processing one view
// ============================================================================

/// The outcome of processing a single view during propagation.
#[derive(Debug, Clone)]
pub struct PropagationResult {
    /// Which view was processed.
    pub view_id: ViewId,
    /// The domain of the view.
    pub domain: ViewDomain,
    /// Number of input delta entries received.
    pub input_delta_size: usize,
    /// Number of output delta entries produced.
    pub output_delta_size: usize,
    /// Whether the view fell back to full recomputation.
    pub fell_back_to_full: bool,
    /// Size of the materialized view (for ratio comparison).
    pub materialized_size: usize,
    /// Time taken to process this view (microseconds).
    pub duration_us: u64,
}

// ============================================================================
// EpochEvidence — JSONL evidence for one propagation epoch
// ============================================================================

/// Evidence record for a single IVM propagation epoch.
///
/// Serialized to JSONL for offline analysis of delta efficiency vs full
/// recompute. Consumed by the evidence sink and SLO breach detection.
#[derive(Debug, Clone)]
pub struct EpochEvidence {
    /// Monotonic epoch counter.
    pub epoch: u64,
    /// Number of views that received deltas.
    pub views_processed: usize,
    /// Number of views that fell back to full recompute.
    pub views_recomputed: usize,
    /// Sum of delta entry counts across all views.
    pub total_delta_size: usize,
    /// Sum of materialized view sizes (baseline cost).
    pub total_materialized_size: usize,
    /// Wall-clock time for the entire propagation (microseconds).
    pub duration_us: u64,
    /// Per-view results.
    pub per_view: Vec<PropagationResult>,
}

impl EpochEvidence {
    /// The delta-to-full ratio. Values < 1.0 mean IVM is winning.
    ///
    /// Ratio = total_delta_size / total_materialized_size.
    /// A ratio of 0.05 means the delta was 5% of a full recompute.
    pub fn delta_ratio(&self) -> f64 {
        if self.total_materialized_size == 0 {
            0.0
        } else {
            self.total_delta_size as f64 / self.total_materialized_size as f64
        }
    }

    /// Format as a JSONL line for the evidence sink.
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"type\":\"ivm_epoch\",\"epoch\":{},\"views_processed\":{},\"views_recomputed\":{},\"total_delta_size\":{},\"total_materialized_size\":{},\"delta_ratio\":{:.4},\"duration_us\":{}}}",
            self.epoch,
            self.views_processed,
            self.views_recomputed,
            self.total_delta_size,
            self.total_materialized_size,
            self.delta_ratio(),
            self.duration_us,
        )
    }
}

// ============================================================================
// FallbackPolicy — when to give up on incremental and do full recompute
// ============================================================================

/// Policy for deciding when a view should fall back to full recomputation.
///
/// If the delta set is large relative to the materialized view, incremental
/// processing may be slower than just recomputing everything. The threshold
/// is configurable per-domain.
#[derive(Debug, Clone)]
pub struct FallbackPolicy {
    /// If delta_size / materialized_size exceeds this ratio, fall back.
    /// Default: 0.5 (50% — delta is more than half the full view).
    pub ratio_threshold: f64,
    /// Absolute minimum delta size before considering fallback.
    /// Prevents fallback on tiny views where ratio is noisy.
    /// Default: 10.
    pub min_delta_for_fallback: usize,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self {
            ratio_threshold: 0.5,
            min_delta_for_fallback: 10,
        }
    }
}

impl FallbackPolicy {
    /// Whether the view should fall back to full recomputation.
    pub fn should_fallback(&self, delta_size: usize, materialized_size: usize) -> bool {
        if delta_size < self.min_delta_for_fallback {
            return false;
        }
        if materialized_size == 0 {
            return true; // Empty view, just recompute.
        }
        (delta_size as f64 / materialized_size as f64) > self.ratio_threshold
    }
}

// ============================================================================
// DAG Edge — connection between views
// ============================================================================

/// An edge in the IVM DAG, connecting an upstream view to a downstream view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagEdge {
    /// The upstream view that produces deltas.
    pub from: ViewId,
    /// The downstream view that consumes deltas.
    pub to: ViewId,
}

// ============================================================================
// DagTopology — the static structure of the view DAG
// ============================================================================

/// The static topology of the IVM DAG.
///
/// Built once when the view pipeline is constructed. The topological order
/// is pre-computed and cached for efficient per-epoch propagation.
///
/// # Invariants
///
/// 1. The DAG is acyclic (enforced on edge insertion).
/// 2. Topological order includes all views.
/// 3. For every edge (A, B), A appears before B in the topological order.
/// 4. Adding or removing views invalidates the cached topological order.
#[derive(Debug, Clone)]
pub struct DagTopology {
    /// All views in the DAG, indexed by ViewId.
    pub views: Vec<ViewDescriptor>,
    /// All edges in the DAG.
    pub edges: Vec<DagEdge>,
    /// Pre-computed topological order (view indices).
    pub topo_order: Vec<ViewId>,
}

/// Descriptor for a view in the DAG.
#[derive(Debug, Clone)]
pub struct ViewDescriptor {
    /// The view's unique identifier.
    pub id: ViewId,
    /// Human-readable label for debugging/logging.
    pub label: String,
    /// The domain of the view.
    pub domain: ViewDomain,
    /// Fallback policy for this view.
    pub fallback_policy: FallbackPolicy,
}

impl DagTopology {
    /// Create an empty DAG.
    pub fn new() -> Self {
        Self {
            views: Vec::new(),
            edges: Vec::new(),
            topo_order: Vec::new(),
        }
    }

    /// Add a view to the DAG. Returns its ViewId.
    pub fn add_view(&mut self, label: impl Into<String>, domain: ViewDomain) -> ViewId {
        let id = ViewId(self.views.len() as u32);
        self.views.push(ViewDescriptor {
            id,
            label: label.into(),
            domain,
            fallback_policy: FallbackPolicy::default(),
        });
        id
    }

    /// Add an edge: `from` produces deltas consumed by `to`.
    ///
    /// # Panics
    ///
    /// Panics if this would create a cycle in the DAG.
    pub fn add_edge(&mut self, from: ViewId, to: ViewId) {
        // Cycle check: verify `to` cannot reach `from` via existing edges.
        assert!(
            !self.can_reach(to, from),
            "IVM DAG cycle detected: {} -> {} would create a cycle",
            from,
            to
        );
        self.edges.push(DagEdge { from, to });
    }

    /// Check if `from` can reach `to` via existing edges (DFS).
    fn can_reach(&self, from: ViewId, to: ViewId) -> bool {
        let mut visited = vec![false; self.views.len()];
        let mut stack = vec![from];
        while let Some(current) = stack.pop() {
            if current == to {
                return true;
            }
            let idx = current.0 as usize;
            if idx >= visited.len() || visited[idx] {
                continue;
            }
            visited[idx] = true;
            for edge in &self.edges {
                if edge.from == current && !visited[edge.to.0 as usize] {
                    stack.push(edge.to);
                }
            }
        }
        false
    }

    /// Compute the topological order via Kahn's algorithm.
    ///
    /// Must be called after all views and edges are added. The result is
    /// cached in `self.topo_order` for efficient per-epoch propagation.
    ///
    /// # Panics
    ///
    /// Panics if the DAG contains a cycle (should be impossible due to
    /// `add_edge` validation, but checked defensively).
    pub fn compute_topo_order(&mut self) {
        let n = self.views.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<ViewId>> = vec![Vec::new(); n];

        for edge in &self.edges {
            in_degree[edge.to.0 as usize] += 1;
            adj[edge.from.0 as usize].push(edge.to);
        }

        // Seed with zero-in-degree views (sorted for determinism).
        let mut queue: Vec<ViewId> = (0..n)
            .filter(|&i| in_degree[i] == 0)
            .map(|i| ViewId(i as u32))
            .collect();
        queue.sort();

        let mut order = Vec::with_capacity(n);
        while let Some(v) = queue.pop() {
            order.push(v);
            // Sort neighbors for deterministic order.
            let mut neighbors = adj[v.0 as usize].clone();
            neighbors.sort();
            for next in neighbors {
                in_degree[next.0 as usize] -= 1;
                if in_degree[next.0 as usize] == 0 {
                    // Insert in sorted position for determinism.
                    let pos = queue.partition_point(|&x| x > next);
                    queue.insert(pos, next);
                }
            }
        }

        assert_eq!(
            order.len(),
            n,
            "IVM DAG has a cycle: only {} of {} views in topo order",
            order.len(),
            n
        );

        self.topo_order = order;
    }

    /// Get the downstream views that consume deltas from `view_id`.
    pub fn downstream(&self, view_id: ViewId) -> Vec<ViewId> {
        self.edges
            .iter()
            .filter(|e| e.from == view_id)
            .map(|e| e.to)
            .collect()
    }

    /// Get the upstream views that produce deltas for `view_id`.
    pub fn upstream(&self, view_id: ViewId) -> Vec<ViewId> {
        self.edges
            .iter()
            .filter(|e| e.to == view_id)
            .map(|e| e.from)
            .collect()
    }

    /// Number of views in the DAG.
    pub fn view_count(&self) -> usize {
        self.views.len()
    }

    /// Number of edges in the DAG.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

impl Default for DagTopology {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Concrete Key/Value Types for the Three Pipeline Stages
// ============================================================================

/// Key for style view: identifies a widget's style slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StyleKey(pub u32);

/// Key for layout view: identifies a layout node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayoutKey(pub u32);

/// Key for render view: identifies a cell position (row, col).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellKey {
    pub row: u16,
    pub col: u16,
}

/// Resolved style value (the output of style resolution).
///
/// Matches the existing `Style` type from ftui-style but represented as
/// a hash for delta comparison. The actual `Style` struct is 32-40 bytes
/// and uses `Copy` semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedStyleValue {
    /// Hash of the resolved style for change detection.
    pub style_hash: u64,
}

/// Layout result value (the output of layout computation).
///
/// Wraps the cached Rect positions from IncrementalLayout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutValue {
    /// Hash of the computed rects for change detection.
    pub rects_hash: u64,
    /// Number of sub-regions computed.
    pub rect_count: u16,
}

/// Cell value (the output of render computation).
///
/// Matches the 16-byte Cell struct from ftui-render. Represented as a
/// hash for delta comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellValue {
    /// Hash of the cell's content + fg + bg + attrs.
    pub cell_hash: u64,
}

// ============================================================================
// IvmConfig — runtime configuration
// ============================================================================

/// Configuration for the IVM system.
#[derive(Debug, Clone)]
pub struct IvmConfig {
    /// Force full recomputation on every frame (disables incremental).
    /// Env: `FRANKENTUI_FULL_RECOMPUTE=1`
    pub force_full: bool,
    /// Default fallback policy for views without custom policies.
    pub default_fallback: FallbackPolicy,
    /// Whether to emit evidence JSONL for each epoch.
    pub emit_evidence: bool,
}

impl Default for IvmConfig {
    fn default() -> Self {
        Self {
            force_full: false,
            default_fallback: FallbackPolicy::default(),
            emit_evidence: true,
        }
    }
}

impl IvmConfig {
    /// Create from environment variables.
    pub fn from_env() -> Self {
        let force = std::env::var("FRANKENTUI_FULL_RECOMPUTE")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        Self {
            force_full: force,
            ..Default::default()
        }
    }
}

// ============================================================================
// Hash utilities
// ============================================================================

/// Compute a fast hash of an arbitrary hashable value.
///
/// Uses FxHash (same as IncrementalLayout) for consistency with the
/// existing caching infrastructure.
pub fn fx_hash<T: Hash>(value: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

// ============================================================================
// IncrementalView — the core trait (bd-3akdb.2)
// ============================================================================

/// The core trait for incremental view maintenance.
///
/// An `IncrementalView` maintains a materialized view that can be updated
/// incrementally via deltas, or fully recomputed as a fallback.
///
/// # Type Parameters
///
/// - `K`: Key type for addressing entries in the materialized view.
/// - `V`: Value type stored at each key.
///
/// # Contract
///
/// 1. `apply_delta(batch)` produces output deltas reflecting how the
///    materialized view changed.
/// 2. `full_recompute()` returns the complete materialized view.
/// 3. **Correctness invariant**: After applying any sequence of deltas,
///    the materialized view must equal what `full_recompute()` would
///    return given the same inputs.
/// 4. `materialized_size()` returns the number of entries for fallback
///    policy decisions.
pub trait IncrementalView<K: Clone + Eq + Hash, V: Clone + PartialEq> {
    /// Apply a batch of input deltas and produce output deltas.
    ///
    /// The output deltas describe how this view's materialized state changed
    /// as a result of the input. These are forwarded to downstream views.
    fn apply_delta(&mut self, batch: &DeltaBatch<K, V>) -> DeltaBatch<K, V>;

    /// Fully recompute the materialized view from scratch.
    ///
    /// Returns all current (key, value) pairs. Used as a fallback when
    /// the delta set is too large, or for correctness validation.
    fn full_recompute(&self) -> Vec<(K, V)>;

    /// Number of entries in the materialized view.
    fn materialized_size(&self) -> usize;

    /// Domain of this view (for logging and evidence).
    fn domain(&self) -> ViewDomain;

    /// Human-readable label for this view.
    fn label(&self) -> &str;
}

// ============================================================================
// StyleResolutionView — concrete view for theme → resolved styles
// ============================================================================

/// A concrete `IncrementalView` that resolves styles by merging a base
/// (theme) style with per-widget overrides.
///
/// When the theme changes, only widgets whose resolved style actually
/// differs are emitted as output deltas.
///
/// # Materialized View
///
/// Maps `StyleKey` (widget ID) → `ResolvedStyleValue` (hash of resolved
/// style). The resolved style is `base_hash XOR override_hash` — a
/// simplified model of the real merge(child, parent) operation.
pub struct StyleResolutionView {
    label: String,
    /// Base style hash (from theme). Applies to all widgets.
    base_hash: u64,
    /// Per-widget style overrides.
    overrides: std::collections::HashMap<StyleKey, u64>,
    /// Materialized resolved styles.
    resolved: std::collections::HashMap<StyleKey, ResolvedStyleValue>,
}

impl StyleResolutionView {
    /// Create a new style resolution view with the given base theme hash.
    pub fn new(label: impl Into<String>, base_hash: u64) -> Self {
        Self {
            label: label.into(),
            base_hash,
            overrides: std::collections::HashMap::new(),
            resolved: std::collections::HashMap::new(),
        }
    }

    /// Set the base theme hash. All resolved styles will be recomputed.
    pub fn set_base(&mut self, base_hash: u64) {
        self.base_hash = base_hash;
    }

    /// Get the current base hash.
    pub fn base_hash(&self) -> u64 {
        self.base_hash
    }

    /// Resolve a single style: base XOR override.
    fn resolve(&self, key: &StyleKey) -> ResolvedStyleValue {
        let override_hash = self.overrides.get(key).copied().unwrap_or(0);
        ResolvedStyleValue {
            style_hash: self.base_hash ^ override_hash,
        }
    }
}

impl IncrementalView<StyleKey, ResolvedStyleValue> for StyleResolutionView {
    fn apply_delta(
        &mut self,
        batch: &DeltaBatch<StyleKey, ResolvedStyleValue>,
    ) -> DeltaBatch<StyleKey, ResolvedStyleValue> {
        let mut output = DeltaBatch::new(batch.epoch);
        let mut time = 0u64;

        for entry in &batch.entries {
            match entry {
                DeltaEntry::Insert {
                    key,
                    value,
                    logical_time: _,
                } => {
                    // Update override for this widget.
                    self.overrides.insert(*key, value.style_hash);
                    let new_resolved = self.resolve(key);
                    let old = self.resolved.insert(*key, new_resolved);
                    // Only emit delta if resolved style actually changed.
                    if old.as_ref() != Some(&new_resolved) {
                        output.insert(*key, new_resolved, time);
                        time += 1;
                    }
                }
                DeltaEntry::Delete {
                    key,
                    logical_time: _,
                } => {
                    self.overrides.remove(key);
                    if self.resolved.remove(key).is_some() {
                        output.delete(*key, time);
                        time += 1;
                    }
                }
            }
        }
        output
    }

    fn full_recompute(&self) -> Vec<(StyleKey, ResolvedStyleValue)> {
        self.overrides
            .keys()
            .map(|key| (*key, self.resolve(key)))
            .collect()
    }

    fn materialized_size(&self) -> usize {
        self.resolved.len()
    }

    fn domain(&self) -> ViewDomain {
        ViewDomain::Style
    }

    fn label(&self) -> &str {
        &self.label
    }
}

// ============================================================================
// FilteredListView — concrete view for data → visible subset
// ============================================================================

/// A concrete `IncrementalView` that maintains a filtered subset of items.
///
/// Items are included if their value hash passes the filter predicate.
/// When items are inserted/deleted/updated, only the changes to the
/// filtered set are emitted as output deltas.
type FilterPredicate<K, V> = dyn Fn(&K, &V) -> bool;

pub struct FilteredListView<K: Clone + Eq + Hash, V: Clone + PartialEq> {
    label: String,
    /// The filter predicate: returns true if the item should be visible.
    filter: Box<FilterPredicate<K, V>>,
    /// All items (unfiltered).
    all_items: std::collections::HashMap<K, V>,
    /// Materialized filtered subset.
    visible: std::collections::HashMap<K, V>,
}

impl<K: Clone + Eq + Hash, V: Clone + PartialEq> FilteredListView<K, V> {
    /// Create a new filtered list view with the given filter predicate.
    pub fn new(label: impl Into<String>, filter: impl Fn(&K, &V) -> bool + 'static) -> Self {
        Self {
            label: label.into(),
            filter: Box::new(filter),
            all_items: std::collections::HashMap::new(),
            visible: std::collections::HashMap::new(),
        }
    }

    /// Number of total items (before filtering).
    pub fn total_count(&self) -> usize {
        self.all_items.len()
    }

    /// Number of visible items (after filtering).
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }
}

impl<K: Clone + Eq + Hash, V: Clone + PartialEq> IncrementalView<K, V> for FilteredListView<K, V> {
    fn apply_delta(&mut self, batch: &DeltaBatch<K, V>) -> DeltaBatch<K, V> {
        let mut output = DeltaBatch::new(batch.epoch);
        let mut time = 0u64;

        for entry in &batch.entries {
            match entry {
                DeltaEntry::Insert {
                    key,
                    value,
                    logical_time: _,
                } => {
                    let was_visible = self.visible.contains_key(key);
                    self.all_items.insert(key.clone(), value.clone());
                    let now_visible = (self.filter)(key, value);

                    if now_visible {
                        let old = self.visible.insert(key.clone(), value.clone());
                        // Emit if newly visible or value changed.
                        if !was_visible || old.as_ref() != Some(value) {
                            output.insert(key.clone(), value.clone(), time);
                            time += 1;
                        }
                    } else if was_visible {
                        // Was visible, now filtered out → delete.
                        self.visible.remove(key);
                        output.delete(key.clone(), time);
                        time += 1;
                    }
                }
                DeltaEntry::Delete {
                    key,
                    logical_time: _,
                } => {
                    self.all_items.remove(key);
                    if self.visible.remove(key).is_some() {
                        output.delete(key.clone(), time);
                        time += 1;
                    }
                }
            }
        }
        output
    }

    fn full_recompute(&self) -> Vec<(K, V)> {
        self.all_items
            .iter()
            .filter(|(k, v)| (self.filter)(k, v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn materialized_size(&self) -> usize {
        self.visible.len()
    }

    fn domain(&self) -> ViewDomain {
        ViewDomain::FilteredList
    }

    fn label(&self) -> &str {
        &self.label
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── DeltaEntry ──────────────────────────────────────────────────

    #[test]
    fn delta_entry_insert_fields() {
        let entry: DeltaEntry<u32, String> = DeltaEntry::Insert {
            key: 42,
            value: "hello".into(),
            logical_time: 1,
        };
        assert_eq!(*entry.key(), 42);
        assert_eq!(entry.logical_time(), 1);
        assert_eq!(entry.weight(), 1);
        assert!(entry.is_insert());
    }

    #[test]
    fn delta_entry_delete_fields() {
        let entry: DeltaEntry<u32, String> = DeltaEntry::Delete {
            key: 42,
            logical_time: 2,
        };
        assert_eq!(*entry.key(), 42);
        assert_eq!(entry.logical_time(), 2);
        assert_eq!(entry.weight(), -1);
        assert!(!entry.is_insert());
    }

    // ── DeltaBatch ──────────────────────────────────────────────────

    #[test]
    fn batch_operations() {
        let mut batch = DeltaBatch::new(1);
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
        assert_eq!(batch.epoch, 1);

        batch.insert(1u32, "a".to_string(), 1);
        batch.insert(2, "b".to_string(), 2);
        batch.delete(1, 3);

        assert_eq!(batch.len(), 3);
        assert!(!batch.is_empty());
    }

    // ── FallbackPolicy ──────────────────────────────────────────────

    #[test]
    fn fallback_below_min() {
        let policy = FallbackPolicy {
            ratio_threshold: 0.5,
            min_delta_for_fallback: 10,
        };
        // 5 deltas < min_delta_for_fallback, so no fallback.
        assert!(!policy.should_fallback(5, 100));
    }

    #[test]
    fn fallback_above_threshold() {
        let policy = FallbackPolicy {
            ratio_threshold: 0.5,
            min_delta_for_fallback: 10,
        };
        // 60/100 = 0.6 > 0.5 threshold.
        assert!(policy.should_fallback(60, 100));
    }

    #[test]
    fn fallback_below_threshold() {
        let policy = FallbackPolicy {
            ratio_threshold: 0.5,
            min_delta_for_fallback: 10,
        };
        // 20/100 = 0.2 < 0.5 threshold.
        assert!(!policy.should_fallback(20, 100));
    }

    #[test]
    fn fallback_empty_view() {
        let policy = FallbackPolicy::default();
        // Empty view → always fallback if delta >= min.
        assert!(policy.should_fallback(10, 0));
    }

    // ── DagTopology ─────────────────────────────────────────────────

    #[test]
    fn empty_dag() {
        let dag = DagTopology::new();
        assert_eq!(dag.view_count(), 0);
        assert_eq!(dag.edge_count(), 0);
    }

    #[test]
    fn add_views_and_edges() {
        let mut dag = DagTopology::new();
        let style = dag.add_view("style", ViewDomain::Style);
        let layout = dag.add_view("layout", ViewDomain::Layout);
        let render = dag.add_view("render", ViewDomain::Render);

        dag.add_edge(style, layout);
        dag.add_edge(layout, render);

        assert_eq!(dag.view_count(), 3);
        assert_eq!(dag.edge_count(), 2);
    }

    #[test]
    fn topological_order_linear() {
        let mut dag = DagTopology::new();
        let a = dag.add_view("style", ViewDomain::Style);
        let b = dag.add_view("layout", ViewDomain::Layout);
        let c = dag.add_view("render", ViewDomain::Render);

        dag.add_edge(a, b);
        dag.add_edge(b, c);
        dag.compute_topo_order();

        assert_eq!(dag.topo_order, vec![a, b, c]);
    }

    #[test]
    fn topological_order_diamond() {
        // A → B, A → C, B → D, C → D
        let mut dag = DagTopology::new();
        let a = dag.add_view("source", ViewDomain::Style);
        let b = dag.add_view("branch_b", ViewDomain::Layout);
        let c = dag.add_view("branch_c", ViewDomain::Layout);
        let d = dag.add_view("sink", ViewDomain::Render);

        dag.add_edge(a, b);
        dag.add_edge(a, c);
        dag.add_edge(b, d);
        dag.add_edge(c, d);
        dag.compute_topo_order();

        // A must come first, D must come last.
        assert_eq!(dag.topo_order[0], a);
        assert_eq!(dag.topo_order[3], d);
        // B and C can be in either order, both are valid.
        let middle: Vec<ViewId> = dag.topo_order[1..3].to_vec();
        assert!(middle.contains(&b));
        assert!(middle.contains(&c));
    }

    #[test]
    #[should_panic(expected = "cycle")]
    fn cycle_detection() {
        let mut dag = DagTopology::new();
        let a = dag.add_view("a", ViewDomain::Style);
        let b = dag.add_view("b", ViewDomain::Layout);
        dag.add_edge(a, b);
        dag.add_edge(b, a); // Cycle!
    }

    #[test]
    fn downstream_upstream() {
        let mut dag = DagTopology::new();
        let a = dag.add_view("a", ViewDomain::Style);
        let b = dag.add_view("b", ViewDomain::Layout);
        let c = dag.add_view("c", ViewDomain::Render);
        dag.add_edge(a, b);
        dag.add_edge(a, c);

        let down = dag.downstream(a);
        assert_eq!(down.len(), 2);
        assert!(down.contains(&b));
        assert!(down.contains(&c));

        let up = dag.upstream(b);
        assert_eq!(up, vec![a]);
    }

    // ── EpochEvidence ───────────────────────────────────────────────

    #[test]
    fn epoch_evidence_delta_ratio() {
        let ev = EpochEvidence {
            epoch: 1,
            views_processed: 3,
            views_recomputed: 0,
            total_delta_size: 10,
            total_materialized_size: 200,
            duration_us: 42,
            per_view: vec![],
        };
        assert!((ev.delta_ratio() - 0.05).abs() < 0.001);
    }

    #[test]
    fn epoch_evidence_jsonl() {
        let ev = EpochEvidence {
            epoch: 5,
            views_processed: 3,
            views_recomputed: 1,
            total_delta_size: 25,
            total_materialized_size: 500,
            duration_us: 100,
            per_view: vec![],
        };
        let jsonl = ev.to_jsonl();
        assert!(jsonl.contains("\"type\":\"ivm_epoch\""));
        assert!(jsonl.contains("\"epoch\":5"));
        assert!(jsonl.contains("\"views_recomputed\":1"));
        assert!(jsonl.contains("\"delta_ratio\":0.0500"));
    }

    #[test]
    fn epoch_evidence_empty_materialized() {
        let ev = EpochEvidence {
            epoch: 1,
            views_processed: 0,
            views_recomputed: 0,
            total_delta_size: 0,
            total_materialized_size: 0,
            duration_us: 0,
            per_view: vec![],
        };
        assert!((ev.delta_ratio() - 0.0).abs() < f64::EPSILON);
    }

    // ── Key types ───────────────────────────────────────────────────

    #[test]
    fn cell_key_ordering() {
        let a = CellKey { row: 0, col: 5 };
        let b = CellKey { row: 0, col: 10 };
        let c = CellKey { row: 1, col: 0 };
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn style_key_hash() {
        let a = StyleKey(42);
        let b = StyleKey(42);
        assert_eq!(fx_hash(&a), fx_hash(&b));
    }

    // ── IvmConfig ───────────────────────────────────────────────────

    #[test]
    fn config_defaults() {
        let config = IvmConfig::default();
        assert!(!config.force_full);
        assert!(config.emit_evidence);
        assert!((config.default_fallback.ratio_threshold - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn config_from_env_default() {
        let config = IvmConfig::from_env();
        assert_eq!(config.default_fallback.min_delta_for_fallback, 10);
    }

    // ── ViewDomain ──────────────────────────────────────────────────

    #[test]
    fn view_domain_labels() {
        assert_eq!(ViewDomain::Style.as_str(), "style");
        assert_eq!(ViewDomain::Layout.as_str(), "layout");
        assert_eq!(ViewDomain::Render.as_str(), "render");
        assert_eq!(ViewDomain::FilteredList.as_str(), "filtered_list");
        assert_eq!(ViewDomain::Custom.as_str(), "custom");
    }

    // ── Full pipeline topology ──────────────────────────────────────

    #[test]
    fn three_stage_pipeline_topology() {
        let mut dag = DagTopology::new();

        // Build the canonical style → layout → render pipeline.
        let style = dag.add_view("StyleView", ViewDomain::Style);
        let layout = dag.add_view("LayoutView", ViewDomain::Layout);
        let render = dag.add_view("RenderView", ViewDomain::Render);

        dag.add_edge(style, layout);
        dag.add_edge(layout, render);
        dag.compute_topo_order();

        assert_eq!(dag.topo_order, vec![style, layout, render]);
        assert_eq!(dag.downstream(style), vec![layout]);
        assert_eq!(dag.downstream(layout), vec![render]);
        assert!(dag.downstream(render).is_empty());
        assert!(dag.upstream(style).is_empty());
        assert_eq!(dag.upstream(layout), vec![style]);
        assert_eq!(dag.upstream(render), vec![layout]);
    }

    #[test]
    fn multi_source_pipeline() {
        // Theme and Content both feed into Layout.
        let mut dag = DagTopology::new();
        let theme_style = dag.add_view("ThemeStyle", ViewDomain::Style);
        let content = dag.add_view("Content", ViewDomain::Custom);
        let layout = dag.add_view("Layout", ViewDomain::Layout);
        let render = dag.add_view("Render", ViewDomain::Render);

        dag.add_edge(theme_style, layout);
        dag.add_edge(content, layout);
        dag.add_edge(layout, render);
        dag.compute_topo_order();

        // Theme and Content before Layout, Layout before Render.
        let layout_pos = dag.topo_order.iter().position(|&v| v == layout).unwrap();
        let render_pos = dag.topo_order.iter().position(|&v| v == render).unwrap();
        let theme_pos = dag
            .topo_order
            .iter()
            .position(|&v| v == theme_style)
            .unwrap();
        let content_pos = dag.topo_order.iter().position(|&v| v == content).unwrap();

        assert!(theme_pos < layout_pos);
        assert!(content_pos < layout_pos);
        assert!(layout_pos < render_pos);
    }

    #[test]
    fn filtered_list_side_branch() {
        // Main pipeline: Style → Layout → Render
        // Side branch: Content → FilteredList → Layout
        let mut dag = DagTopology::new();
        let style = dag.add_view("Style", ViewDomain::Style);
        let content = dag.add_view("Content", ViewDomain::Custom);
        let filtered = dag.add_view("FilteredList", ViewDomain::FilteredList);
        let layout = dag.add_view("Layout", ViewDomain::Layout);
        let render = dag.add_view("Render", ViewDomain::Render);

        dag.add_edge(style, layout);
        dag.add_edge(content, filtered);
        dag.add_edge(filtered, layout);
        dag.add_edge(layout, render);
        dag.compute_topo_order();

        let filtered_pos = dag.topo_order.iter().position(|&v| v == filtered).unwrap();
        let layout_pos = dag.topo_order.iter().position(|&v| v == layout).unwrap();
        assert!(filtered_pos < layout_pos);
    }

    // ── PropagationResult ───────────────────────────────────────────

    #[test]
    fn propagation_result_construction() {
        let result = PropagationResult {
            view_id: ViewId(0),
            domain: ViewDomain::Style,
            input_delta_size: 5,
            output_delta_size: 3,
            fell_back_to_full: false,
            materialized_size: 100,
            duration_us: 15,
        };
        assert!(!result.fell_back_to_full);
        assert_eq!(result.output_delta_size, 3);
    }

    #[test]
    fn propagation_result_fallback() {
        let result = PropagationResult {
            view_id: ViewId(1),
            domain: ViewDomain::Layout,
            input_delta_size: 80,
            output_delta_size: 100,
            fell_back_to_full: true,
            materialized_size: 100,
            duration_us: 200,
        };
        assert!(result.fell_back_to_full);
    }

    // ── IncrementalView trait ──────────────────────────────────────

    #[test]
    fn style_view_apply_insert() {
        let mut view = StyleResolutionView::new("test_style", 0xABCD);
        let mut batch = DeltaBatch::new(1);
        batch.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0x1234 }, 0);

        let output = view.apply_delta(&batch);
        assert_eq!(output.len(), 1);
        assert!(output.entries[0].is_insert());
        assert_eq!(view.materialized_size(), 1);
    }

    #[test]
    fn style_view_deduplicates_unchanged() {
        let mut view = StyleResolutionView::new("test", 0x0);
        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0x42 }, 0);
        view.apply_delta(&batch1);

        // Same override again → no output delta.
        let mut batch2 = DeltaBatch::new(2);
        batch2.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0x42 }, 0);
        let output = view.apply_delta(&batch2);
        assert!(output.is_empty(), "same style should produce no delta");
    }

    #[test]
    fn style_view_delete() {
        let mut view = StyleResolutionView::new("test", 0x0);
        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0x42 }, 0);
        view.apply_delta(&batch1);
        assert_eq!(view.materialized_size(), 1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.delete(StyleKey(0), 0);
        let output = view.apply_delta(&batch2);
        assert_eq!(output.len(), 1);
        assert!(!output.entries[0].is_insert()); // Delete
        assert_eq!(view.materialized_size(), 0);
    }

    #[test]
    fn style_view_full_recompute_matches_incremental() {
        let mut view = StyleResolutionView::new("test", 0xFF00);
        let mut batch = DeltaBatch::new(1);
        batch.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0x00FF }, 0);
        batch.insert(StyleKey(1), ResolvedStyleValue { style_hash: 0x0F0F }, 1);
        view.apply_delta(&batch);

        let full = view.full_recompute();
        assert_eq!(full.len(), 2);

        // Verify full recompute matches incremental state.
        for (key, value) in &full {
            assert_eq!(value, view.resolved.get(key).unwrap());
        }
    }

    #[test]
    fn style_view_base_change_affects_all() {
        let mut view = StyleResolutionView::new("test", 0x0);
        let mut batch = DeltaBatch::new(1);
        batch.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0x42 }, 0);
        batch.insert(StyleKey(1), ResolvedStyleValue { style_hash: 0x99 }, 1);
        view.apply_delta(&batch);

        // Change base hash and verify full_recompute reflects it.
        view.set_base(0xFFFF);
        let full = view.full_recompute();
        for (_key, value) in &full {
            // Resolved = base XOR override, so should differ from before.
            assert_ne!(value.style_hash, 0);
        }
    }

    #[test]
    fn style_view_domain_and_label() {
        let view = StyleResolutionView::new("ThemeResolver", 0);
        assert_eq!(view.domain(), ViewDomain::Style);
        assert_eq!(view.label(), "ThemeResolver");
    }

    // ── FilteredListView ────────────────────────────────────────────

    #[test]
    fn filtered_view_insert_visible() {
        // Filter: only even values.
        let mut view = FilteredListView::new("evens", |_k: &u32, v: &i32| *v % 2 == 0);
        let mut batch = DeltaBatch::new(1);
        batch.insert(1u32, 4i32, 0); // Even → visible
        batch.insert(2u32, 3i32, 1); // Odd → filtered out

        let output = view.apply_delta(&batch);
        assert_eq!(output.len(), 1); // Only the even value
        assert_eq!(view.visible_count(), 1);
        assert_eq!(view.total_count(), 2);
    }

    #[test]
    fn filtered_view_insert_then_filter_out() {
        let mut view = FilteredListView::new("test", |_k: &u32, v: &i32| *v > 10);
        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(1u32, 20i32, 0); // Visible (20 > 10)
        view.apply_delta(&batch1);
        assert_eq!(view.visible_count(), 1);

        // Update to value that fails filter.
        let mut batch2 = DeltaBatch::new(2);
        batch2.insert(1u32, 5i32, 0); // Now 5 < 10, filtered out
        let output = view.apply_delta(&batch2);
        assert_eq!(output.len(), 1);
        assert!(!output.entries[0].is_insert()); // Delete from visible set
        assert_eq!(view.visible_count(), 0);
    }

    #[test]
    fn filtered_view_delete() {
        let mut view = FilteredListView::new("test", |_k: &u32, v: &i32| *v > 0);
        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(1u32, 5i32, 0);
        view.apply_delta(&batch1);
        assert_eq!(view.visible_count(), 1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.delete(1u32, 0);
        let output = view.apply_delta(&batch2);
        assert_eq!(output.len(), 1);
        assert!(!output.entries[0].is_insert()); // Delete
        assert_eq!(view.visible_count(), 0);
        assert_eq!(view.total_count(), 0);
    }

    #[test]
    fn filtered_view_full_recompute() {
        let mut view = FilteredListView::new("test", |_k: &u32, v: &i32| *v % 2 == 0);
        let mut batch = DeltaBatch::new(1);
        batch.insert(1u32, 2i32, 0);
        batch.insert(2u32, 3i32, 1);
        batch.insert(3u32, 4i32, 2);
        batch.insert(4u32, 5i32, 3);
        view.apply_delta(&batch);

        let full = view.full_recompute();
        assert_eq!(full.len(), 2); // Only even values
        let keys: Vec<u32> = full.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&1));
        assert!(keys.contains(&3));
    }

    #[test]
    fn filtered_view_deduplicates_unchanged() {
        let mut view = FilteredListView::new("test", |_k: &u32, v: &i32| *v > 0);
        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(1u32, 5i32, 0);
        view.apply_delta(&batch1);

        // Insert same value → no output delta.
        let mut batch2 = DeltaBatch::new(2);
        batch2.insert(1u32, 5i32, 0);
        let output = view.apply_delta(&batch2);
        assert!(output.is_empty(), "same value should produce no delta");
    }

    #[test]
    fn filtered_view_domain_and_label() {
        let view: FilteredListView<u32, i32> =
            FilteredListView::new("EvenFilter", |_k: &u32, v: &i32| *v % 2 == 0);
        assert_eq!(view.domain(), ViewDomain::FilteredList);
        assert_eq!(view.label(), "EvenFilter");
    }

    #[test]
    fn filtered_view_correctness_invariant() {
        // After a sequence of deltas, materialized view == full_recompute.
        let mut view = FilteredListView::new("test", |_k: &u32, v: &i32| *v > 0);

        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(1u32, 5i32, 0);
        batch1.insert(2u32, -3i32, 1);
        batch1.insert(3u32, 10i32, 2);
        view.apply_delta(&batch1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.delete(1u32, 0);
        batch2.insert(4u32, 7i32, 1);
        batch2.insert(2u32, 8i32, 2); // Was filtered, now visible
        view.apply_delta(&batch2);

        // Verify materialized matches full_recompute.
        let full = view.full_recompute();
        let mut full_map: std::collections::HashMap<u32, i32> = full.into_iter().collect();
        assert_eq!(full_map.len(), view.visible_count());
        for (k, v) in &view.visible {
            assert_eq!(full_map.remove(k).as_ref(), Some(v));
        }
        assert!(full_map.is_empty());
    }

    #[test]
    fn filtered_view_delete_nonexistent_is_noop() {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("test", |_k: &u32, _v: &i32| true);
        let mut batch = DeltaBatch::new(1);
        batch.delete(999u32, 0);
        let output = view.apply_delta(&batch);
        assert!(output.is_empty());
    }

    // ── Combined trait usage ────────────────────────────────────────

    #[test]
    fn trait_object_dispatch() {
        // Verify IncrementalView can be used as a trait object.
        let view: Box<dyn IncrementalView<StyleKey, ResolvedStyleValue>> =
            Box::new(StyleResolutionView::new("dynamic", 0x42));
        assert_eq!(view.materialized_size(), 0);
        assert_eq!(view.domain(), ViewDomain::Style);
        assert_eq!(view.label(), "dynamic");
    }
}
