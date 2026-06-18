//! Cross-strategy pane-memory telemetry (`bd-25wj7.1`).
//!
//! Pane history can be retained three ways, each buying undo/redo at a different
//! memory price:
//!
//! * **Baseline** — keep only the single live [`PaneTree`]; no undo history at
//!   all. The irreducible floor.
//! * **Checkpointed** — the production [`PaneInteractionTimeline`]: a baseline
//!   snapshot, a periodic checkpoint snapshot, an operation log with per-entry
//!   before/after state hashes, and replay to reconstruct historical states.
//! * **Persistent** — the structurally shared [`PaneVersionStore`]: a `Vec` of
//!   `Arc`-shared version roots where unchanged subtrees are physically reused,
//!   so navigation is `O(1)` index moves with no replay and no hashes.
//!
//! This module turns those three into directly comparable [`PaneMemoryStrategyFootprint`]s
//! and a single [`PaneMemoryComparison`] artifact. Every footprint is decomposed
//! into the *same* retained-state classes — node structs, leaf payload,
//! extension payload, operation log, state hashes, container/metadata — so the
//! dominant memory driver of each strategy is explicit, not buried in one
//! aggregate number. The byte models reuse the canonical timeline methodology
//! (`size_of` struct estimates plus measured string payload bytes), so the
//! comparison is deterministic and reproducible: identical inputs yield
//! byte-identical reports, suitable for CI regression artifacts and as concrete
//! baselines for the bounded-retention (`bd-25wj7.2`) and churn-reduction
//! (`bd-25wj7.3`) work that depends on this bead.
//!
//! The retained-state model lives here in the library so it is pure and
//! unit-testable; *transient* allocation counts (which need allocator
//! instrumentation) are captured by the `pane_memory_telemetry` bench, which
//! embeds this comparison into its emitted JSON manifest.

use crate::pane::{
    PaneInteractionTimeline, PaneNodeKind, PaneNodeRecord, PaneTree, PaneTreeSnapshot,
};
use crate::pane_persistent::{PaneVersionStore, string_map_payload_bytes};

/// Schema version for the emitted memory-telemetry artifact. Bump on any
/// breaking change to the footprint/comparison field layout.
pub const PANE_MEMORY_TELEMETRY_SCHEMA_VERSION: u16 = 1;

/// The history-retention strategy a [`PaneMemoryStrategyFootprint`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PaneMemoryStrategy {
    /// Keep only the single live tree; no undo history.
    Baseline,
    /// Checkpointed operation-log timeline with replay-based navigation.
    Checkpointed,
    /// Structurally shared persistent version store with `O(1)` navigation.
    Persistent,
}

impl PaneMemoryStrategy {
    /// Stable lower-case identifier for logs and artifact keys.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Checkpointed => "checkpointed",
            Self::Persistent => "persistent",
        }
    }
}

/// The single retained-state class that dominates a strategy's footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PaneMemoryDriver {
    /// Tree node structs (records / `Arc` nodes) retained across versions.
    NodeStructs,
    /// Leaf surface-key payload bytes.
    LeafPayload,
    /// Node + leaf extension-map payload bytes.
    ExtensionPayload,
    /// Operation-log entry structs and operation heap payloads.
    OperationLog,
    /// Per-entry before/after state-hash bytes (replay verification cost).
    StateHashes,
    /// Container struct plus version/checkpoint metadata handles.
    ContainerMetadata,
}

impl PaneMemoryDriver {
    /// Human-readable phrase for the dominant driver.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NodeStructs => "node structs",
            Self::LeafPayload => "leaf payload",
            Self::ExtensionPayload => "extension payload",
            Self::OperationLog => "operation log",
            Self::StateHashes => "state hashes",
            Self::ContainerMetadata => "container/metadata",
        }
    }
}

/// Retained-memory footprint of one strategy, decomposed by retained-state class.
///
/// `total_retained_bytes` is authoritative; the per-class fields sum to it
/// exactly (the last class, `container_and_metadata_bytes`, absorbs the
/// remainder so the decomposition is always faithful).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct PaneMemoryStrategyFootprint {
    /// Which strategy this footprint describes.
    pub strategy: PaneMemoryStrategy,
    /// Count of retained heavy units: live tree (`1`) for baseline, retained
    /// snapshots (baseline + checkpoints) for checkpointed, versions for
    /// persistent.
    pub retained_unit_count: usize,
    /// Total retained tree nodes (logical for checkpointed snapshots; physically
    /// distinct for the persistent store).
    pub retained_node_count: usize,
    /// Tree node struct bytes.
    pub node_struct_bytes: usize,
    /// Leaf surface-key payload bytes.
    pub leaf_payload_bytes: usize,
    /// Node + leaf extension-map payload bytes.
    pub extension_payload_bytes: usize,
    /// Operation-log entry struct + operation heap payload bytes (`0` unless
    /// checkpointed).
    pub operation_payload_bytes: usize,
    /// Per-entry before/after state-hash bytes (`0` unless checkpointed).
    pub state_hash_overhead_bytes: usize,
    /// Container struct plus version/checkpoint metadata bytes (remainder).
    pub container_and_metadata_bytes: usize,
    /// Authoritative total retained bytes.
    pub total_retained_bytes: usize,
    /// The single largest retained-state class.
    pub dominant_driver: PaneMemoryDriver,
}

impl PaneMemoryStrategyFootprint {
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        strategy: PaneMemoryStrategy,
        retained_unit_count: usize,
        retained_node_count: usize,
        node_struct_bytes: usize,
        leaf_payload_bytes: usize,
        extension_payload_bytes: usize,
        operation_payload_bytes: usize,
        state_hash_overhead_bytes: usize,
        total_retained_bytes: usize,
    ) -> Self {
        // The container/metadata class absorbs whatever the authoritative total
        // does not attribute to a concrete class, so the decomposition always
        // sums to `total_retained_bytes`.
        let attributed = node_struct_bytes
            .saturating_add(leaf_payload_bytes)
            .saturating_add(extension_payload_bytes)
            .saturating_add(operation_payload_bytes)
            .saturating_add(state_hash_overhead_bytes);
        let container_and_metadata_bytes = total_retained_bytes.saturating_sub(attributed);
        let dominant_driver = dominant_driver(
            node_struct_bytes,
            leaf_payload_bytes,
            extension_payload_bytes,
            operation_payload_bytes,
            state_hash_overhead_bytes,
            container_and_metadata_bytes,
        );
        Self {
            strategy,
            retained_unit_count,
            retained_node_count,
            node_struct_bytes,
            leaf_payload_bytes,
            extension_payload_bytes,
            operation_payload_bytes,
            state_hash_overhead_bytes,
            container_and_metadata_bytes,
            total_retained_bytes,
            dominant_driver,
        }
    }
}

/// Side-by-side retained-memory comparison across the three strategies.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaneMemoryComparison {
    /// Artifact schema version.
    pub schema_version: u16,
    /// No-history floor: a single live tree.
    pub baseline: PaneMemoryStrategyFootprint,
    /// Production checkpointed timeline.
    pub checkpointed: PaneMemoryStrategyFootprint,
    /// Structurally shared persistent store.
    pub persistent: PaneMemoryStrategyFootprint,
    /// `persistent.total / checkpointed.total` (`<1` ⇒ persistent is leaner).
    pub persistent_over_checkpointed: f64,
    /// `checkpointed.total / baseline.total` (the multiplicative cost of history).
    pub checkpointed_over_baseline: f64,
    /// Strategy retaining the fewest total bytes.
    pub most_compact_strategy: PaneMemoryStrategy,
    /// One-line human-readable takeaway naming each strategy's dominant driver.
    pub summary: String,
}

/// Build a deterministic cross-strategy retained-memory comparison.
///
/// `live` is the current tree the caller would keep with no undo history
/// (the baseline floor). `timeline` and `store` must have been driven through
/// the *same* operation history as `live` for the comparison to be meaningful;
/// the function itself is pure and simply reads each strategy's retained-state
/// model.
#[must_use]
pub fn pane_memory_comparison(
    live: &PaneTree,
    timeline: &PaneInteractionTimeline,
    store: &PaneVersionStore,
) -> PaneMemoryComparison {
    let baseline = baseline_footprint(live);
    let checkpointed = checkpointed_footprint(timeline);
    let persistent = persistent_footprint(store);

    let persistent_over_checkpointed = ratio(
        persistent.total_retained_bytes,
        checkpointed.total_retained_bytes,
    );
    let checkpointed_over_baseline = ratio(
        checkpointed.total_retained_bytes,
        baseline.total_retained_bytes,
    );
    let most_compact_strategy = most_compact(&baseline, &checkpointed, &persistent);

    let summary = format!(
        "baseline: {} tree, {}B (driver {}); checkpointed: {} snapshots / {} entries, {}B (driver {}); \
         persistent: {} versions, {}B (driver {}). persistent/checkpointed={:.2}x, checkpointed/baseline={:.2}x; \
         most compact: {}.",
        baseline.retained_unit_count,
        baseline.total_retained_bytes,
        baseline.dominant_driver.as_str(),
        checkpointed.retained_unit_count,
        timeline.entries.len(),
        checkpointed.total_retained_bytes,
        checkpointed.dominant_driver.as_str(),
        persistent.retained_unit_count,
        persistent.total_retained_bytes,
        persistent.dominant_driver.as_str(),
        persistent_over_checkpointed,
        checkpointed_over_baseline,
        most_compact_strategy.as_str(),
    );

    PaneMemoryComparison {
        schema_version: PANE_MEMORY_TELEMETRY_SCHEMA_VERSION,
        baseline,
        checkpointed,
        persistent,
        persistent_over_checkpointed,
        checkpointed_over_baseline,
        most_compact_strategy,
        summary,
    }
}

/// Bytes of one `u64` state hash; the timeline stores a before/after pair per entry.
const STATE_HASH_BYTES: usize = std::mem::size_of::<u64>();

fn baseline_footprint(live: &PaneTree) -> PaneMemoryStrategyFootprint {
    let snapshot = live.to_snapshot();
    let (leaf_payload_bytes, extension_payload_bytes) = snapshot_payload_bytes(&snapshot);
    let node_struct_bytes = snapshot
        .nodes
        .len()
        .saturating_mul(std::mem::size_of::<PaneNodeRecord>());
    let container = std::mem::size_of::<PaneTreeSnapshot>();
    let total = node_struct_bytes
        .saturating_add(leaf_payload_bytes)
        .saturating_add(extension_payload_bytes)
        .saturating_add(container);
    PaneMemoryStrategyFootprint::assemble(
        PaneMemoryStrategy::Baseline,
        1,
        snapshot.nodes.len(),
        node_struct_bytes,
        leaf_payload_bytes,
        extension_payload_bytes,
        0,
        0,
        total,
    )
}

fn checkpointed_footprint(timeline: &PaneInteractionTimeline) -> PaneMemoryStrategyFootprint {
    let diag = timeline.retention_diagnostics();
    // The entry struct stores the two state hashes inline; surface that cost as
    // its own class and attribute the rest of the entry struct to the op log.
    let state_hash_overhead_bytes = diag.entry_count.saturating_mul(2 * STATE_HASH_BYTES);
    let operation_payload_bytes = diag.retained_operation_payload_bytes.saturating_add(
        diag.estimated_entry_struct_bytes
            .saturating_sub(state_hash_overhead_bytes),
    );
    PaneMemoryStrategyFootprint::assemble(
        PaneMemoryStrategy::Checkpointed,
        diag.retained_snapshot_count,
        diag.retained_snapshot_node_count,
        diag.estimated_snapshot_struct_bytes,
        diag.retained_leaf_payload_bytes,
        diag.retained_extension_payload_bytes,
        operation_payload_bytes,
        state_hash_overhead_bytes,
        diag.estimated_total_retained_bytes,
    )
}

fn persistent_footprint(store: &PaneVersionStore) -> PaneMemoryStrategyFootprint {
    let r = store.retention();
    PaneMemoryStrategyFootprint::assemble(
        PaneMemoryStrategy::Persistent,
        r.version_count,
        r.distinct_node_count,
        r.distinct_struct_bytes,
        r.distinct_leaf_payload_bytes,
        r.distinct_extension_payload_bytes,
        0,
        0,
        r.estimated_total_retained_bytes,
    )
}

/// Sum measured leaf-surface and extension payload bytes over a snapshot's nodes.
fn snapshot_payload_bytes(snapshot: &PaneTreeSnapshot) -> (usize, usize) {
    let mut leaf_payload = 0usize;
    let mut extension_payload = 0usize;
    for record in &snapshot.nodes {
        extension_payload =
            extension_payload.saturating_add(string_map_payload_bytes(&record.extensions));
        if let PaneNodeKind::Leaf(leaf) = &record.kind {
            leaf_payload = leaf_payload.saturating_add(leaf.surface_key.len());
            extension_payload =
                extension_payload.saturating_add(string_map_payload_bytes(&leaf.extensions));
        }
    }
    (leaf_payload, extension_payload)
}

/// Pick the dominant retained-state class. Ties resolve to the earlier class in
/// declaration order (node structs first) for deterministic output.
fn dominant_driver(
    node: usize,
    leaf: usize,
    ext: usize,
    op: usize,
    hash: usize,
    container: usize,
) -> PaneMemoryDriver {
    let classes = [
        (PaneMemoryDriver::NodeStructs, node),
        (PaneMemoryDriver::LeafPayload, leaf),
        (PaneMemoryDriver::ExtensionPayload, ext),
        (PaneMemoryDriver::OperationLog, op),
        (PaneMemoryDriver::StateHashes, hash),
        (PaneMemoryDriver::ContainerMetadata, container),
    ];
    let mut best = classes[0];
    for &candidate in &classes[1..] {
        if candidate.1 > best.1 {
            best = candidate;
        }
    }
    best.0
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn most_compact(
    baseline: &PaneMemoryStrategyFootprint,
    checkpointed: &PaneMemoryStrategyFootprint,
    persistent: &PaneMemoryStrategyFootprint,
) -> PaneMemoryStrategy {
    let mut best = baseline;
    if checkpointed.total_retained_bytes < best.total_retained_bytes {
        best = checkpointed;
    }
    if persistent.total_retained_bytes < best.total_retained_bytes {
        best = persistent;
    }
    best.strategy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{PaneId, PaneLeaf, PaneOperation, PanePlacement, PaneSplitRatio, SplitAxis};
    use crate::pane_persistent::VersionedPaneTree;

    fn ratio_of(n: u32, d: u32) -> PaneSplitRatio {
        PaneSplitRatio::new(n, d).expect("valid ratio")
    }

    /// Drive all three strategies through an identical operation history and
    /// return `(live_tree, timeline, store)` ready to compare.
    fn drive_workload() -> (PaneTree, PaneInteractionTimeline, PaneVersionStore) {
        let mut ops: Vec<PaneOperation> = vec![
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Horizontal,
                ratio: ratio_of(1, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("b"),
            },
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Vertical,
                ratio: ratio_of(2, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("c"),
            },
        ];
        // Resize storm on the split created by the second op (id 4).
        let split = PaneId::new(4).expect("valid id");
        for n in 1..=6u32 {
            ops.push(PaneOperation::SetSplitRatio {
                split,
                ratio: ratio_of(n, 1),
            });
        }

        let mut tree = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::default();
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        for (i, op) in ops.iter().enumerate() {
            let id = i as u64;
            timeline
                .apply_and_record(&mut tree, id, id, op.clone())
                .expect("timeline apply");
            store.apply(op).expect("store apply");
        }
        (tree, timeline, store)
    }

    fn assert_footprint_sums(f: &PaneMemoryStrategyFootprint) {
        let class_sum = f.node_struct_bytes
            + f.leaf_payload_bytes
            + f.extension_payload_bytes
            + f.operation_payload_bytes
            + f.state_hash_overhead_bytes
            + f.container_and_metadata_bytes;
        assert_eq!(
            class_sum, f.total_retained_bytes,
            "{:?} class decomposition must sum to the authoritative total",
            f.strategy
        );
    }

    #[test]
    fn comparison_is_deterministic() {
        let (t1, tl1, s1) = drive_workload();
        let (t2, tl2, s2) = drive_workload();
        assert_eq!(
            pane_memory_comparison(&t1, &tl1, &s1),
            pane_memory_comparison(&t2, &tl2, &s2)
        );
    }

    #[test]
    fn every_footprint_decomposition_is_faithful() {
        let (tree, timeline, store) = drive_workload();
        let cmp = pane_memory_comparison(&tree, &timeline, &store);
        assert_footprint_sums(&cmp.baseline);
        assert_footprint_sums(&cmp.checkpointed);
        assert_footprint_sums(&cmp.persistent);
        assert_eq!(cmp.schema_version, PANE_MEMORY_TELEMETRY_SCHEMA_VERSION);
    }

    #[test]
    fn baseline_is_the_floor_and_history_costs_more() {
        let (tree, timeline, store) = drive_workload();
        let cmp = pane_memory_comparison(&tree, &timeline, &store);
        // Retaining history (either way) never costs less than one live tree.
        assert!(cmp.checkpointed.total_retained_bytes > cmp.baseline.total_retained_bytes);
        assert!(cmp.persistent.total_retained_bytes > cmp.baseline.total_retained_bytes);
        assert_eq!(cmp.most_compact_strategy, PaneMemoryStrategy::Baseline);
        assert!(cmp.checkpointed_over_baseline > 1.0);
    }

    #[test]
    fn only_checkpointed_pays_state_hash_and_op_log_overhead() {
        let (tree, timeline, store) = drive_workload();
        let cmp = pane_memory_comparison(&tree, &timeline, &store);
        // The persistent store navigates by index: no operation log, no hashes.
        assert_eq!(cmp.persistent.state_hash_overhead_bytes, 0);
        assert_eq!(cmp.persistent.operation_payload_bytes, 0);
        assert_eq!(cmp.baseline.state_hash_overhead_bytes, 0);
        assert_eq!(cmp.baseline.operation_payload_bytes, 0);
        // 8 ops × 2 hashes × 8 bytes.
        assert_eq!(
            cmp.checkpointed.state_hash_overhead_bytes,
            timeline.entries.len() * 2 * STATE_HASH_BYTES
        );
        assert!(cmp.checkpointed.state_hash_overhead_bytes > 0);
    }

    #[test]
    fn checkpointed_defers_nodes_while_persistent_materializes_versions() {
        let (tree, timeline, store) = drive_workload();
        let cmp = pane_memory_comparison(&tree, &timeline, &store);
        // The two history strategies sit on opposite sides of the memory/replay
        // trade. For a short history (below the checkpoint interval) the
        // checkpointed path retains almost no snapshot nodes — it leans on its
        // operation log + state hashes and reconstructs by replay — whereas the
        // persistent store materializes every version's distinct nodes so it can
        // navigate in O(1).
        assert!(cmp.persistent.retained_node_count > cmp.checkpointed.retained_node_count);
        assert!(cmp.checkpointed.operation_payload_bytes > 0);
        assert_eq!(cmp.persistent.operation_payload_bytes, 0);
        // The comparison ratios are populated and finite.
        assert!(cmp.persistent_over_checkpointed > 0.0);
        assert!(cmp.persistent_over_checkpointed.is_finite());
    }

    #[test]
    fn dominant_driver_prefers_earlier_class_on_ties() {
        // Equal node and leaf bytes ⇒ node structs win (earlier in order).
        assert_eq!(
            dominant_driver(10, 10, 0, 0, 0, 0),
            PaneMemoryDriver::NodeStructs
        );
        // A strictly larger later class wins outright.
        assert_eq!(
            dominant_driver(1, 1, 1, 99, 1, 1),
            PaneMemoryDriver::OperationLog
        );
    }
}
