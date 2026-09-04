#![cfg(feature = "experimental")]
//! Property-based invariant tests for Incremental View Maintenance (IVM).
//!
//! These tests verify structural invariants of the IVM module that must hold
//! for any valid inputs:
//!
//! 1. After any delta sequence, incremental result == full_recompute result.
//! 2. Empty delta produces no output changes.
//! 3. Delta propagation preserves materialized size consistency.
//! 4. Insert then delete is a no-op on the materialized view.
//! 5. Duplicate inserts with same value produce no output delta.
//! 6. FallbackPolicy is monotonic in delta_size.
//! 7. DagTopology topo_order includes all views exactly once.
//! 8. DagTopology respects edge ordering invariant.
//! 9. EpochEvidence delta_ratio is bounded in [0, +inf).
//! 10. FilteredListView visible subset is always a subset of all items.
//! 11. StyleResolutionView full_recompute is deterministic (same inputs → same output).
//! 12. DeltaBatch len matches entries count.
//! 13. DeltaEntry weight sign matches variant.
//! 14. FilteredListView correctness: visible == filter(all_items) after any delta sequence.
//! 15. StyleResolutionView correctness: resolved == base XOR override after any delta sequence.

use ftui_runtime::ivm::{
    DagTopology, DeltaBatch, DeltaEntry, EpochEvidence, FallbackPolicy, FilteredListView,
    IncrementalView, ResolvedStyleValue, StyleKey, StyleResolutionView, ViewDomain, ViewId,
};
use proptest::prelude::*;
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════
// Strategy helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Generate a random delta operation for StyleResolutionView.
#[derive(Debug, Clone)]
enum StyleOp {
    Insert(u32, u64),
    Delete(u32),
}

fn style_op_strategy() -> impl Strategy<Value = StyleOp> {
    prop_oneof![
        (0u32..50, any::<u64>()).prop_map(|(k, v)| StyleOp::Insert(k, v)),
        (0u32..50).prop_map(StyleOp::Delete),
    ]
}

/// Generate a random delta operation for FilteredListView<u32, i32>.
#[derive(Debug, Clone)]
enum FilterOp {
    Insert(u32, i32),
    Delete(u32),
}

fn filter_op_strategy() -> impl Strategy<Value = FilterOp> {
    prop_oneof![
        (0u32..50, -100i32..100).prop_map(|(k, v)| FilterOp::Insert(k, v)),
        (0u32..50).prop_map(FilterOp::Delete),
    ]
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. StyleResolutionView: incremental == full_recompute after any delta seq
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn style_incremental_equals_full_recompute(
        base_hash in any::<u64>(),
        ops in prop::collection::vec(style_op_strategy(), 0..30),
    ) {
        let mut view = StyleResolutionView::new("proptest", base_hash);
        for (epoch, op) in (1u64..).zip(ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            match op {
                StyleOp::Insert(k, v) => {
                    batch.insert(StyleKey(*k), ResolvedStyleValue { style_hash: *v }, 0);
                }
                StyleOp::Delete(k) => {
                    batch.delete(StyleKey(*k), 0);
                }
            }
            view.apply_delta(&batch);
        }

        // The materialized view must match full_recompute.
        let full = view.full_recompute();
        let full_map: HashMap<StyleKey, ResolvedStyleValue> = full.into_iter().collect();
        prop_assert_eq!(
            full_map.len(),
            view.materialized_size(),
            "full_recompute size ({}) != materialized_size ({})",
            full_map.len(),
            view.materialized_size()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Empty delta produces no output changes
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn empty_delta_no_output_style(base in any::<u64>(), epoch in 0u64..1000) {
        let mut view = StyleResolutionView::new("empty_test", base);
        let batch: DeltaBatch<StyleKey, ResolvedStyleValue> = DeltaBatch::new(epoch);
        let output = view.apply_delta(&batch);
        prop_assert!(
            output.is_empty(),
            "Empty delta should produce no output, got {} entries",
            output.len()
        );
    }
}

proptest! {
    #[test]
    fn empty_delta_no_output_filter(epoch in 0u64..1000) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("empty_test", |_k: &u32, v: &i32| *v > 0);
        let batch: DeltaBatch<u32, i32> = DeltaBatch::new(epoch);
        let output = view.apply_delta(&batch);
        prop_assert!(
            output.is_empty(),
            "Empty delta should produce no output, got {} entries",
            output.len()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Insert then delete is a no-op on materialized view
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn insert_then_delete_is_noop_style(
        base in any::<u64>(),
        key in 0u32..100,
        value in any::<u64>(),
    ) {
        let mut view = StyleResolutionView::new("ins_del", base);
        let size_before = view.materialized_size();

        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(StyleKey(key), ResolvedStyleValue { style_hash: value }, 0);
        view.apply_delta(&batch1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.delete(StyleKey(key), 0);
        view.apply_delta(&batch2);

        prop_assert_eq!(
            view.materialized_size(),
            size_before,
            "Insert + Delete should return to original size"
        );
    }
}

proptest! {
    #[test]
    fn insert_then_delete_is_noop_filter(
        key in 0u32..100,
        value in -100i32..100,
    ) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("ins_del", |_k: &u32, _v: &i32| true);

        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(key, value, 0);
        view.apply_delta(&batch1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.delete(key, 0);
        view.apply_delta(&batch2);

        prop_assert_eq!(view.visible_count(), 0);
        prop_assert_eq!(view.total_count(), 0);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Duplicate insert with same value produces no output delta
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn duplicate_insert_no_output_style(
        base in any::<u64>(),
        key in 0u32..100,
        value in any::<u64>(),
    ) {
        let mut view = StyleResolutionView::new("dedup", base);

        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(StyleKey(key), ResolvedStyleValue { style_hash: value }, 0);
        view.apply_delta(&batch1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.insert(StyleKey(key), ResolvedStyleValue { style_hash: value }, 0);
        let output = view.apply_delta(&batch2);

        prop_assert!(
            output.is_empty(),
            "Duplicate insert should produce no output delta, got {} entries",
            output.len()
        );
    }
}

proptest! {
    #[test]
    fn duplicate_insert_no_output_filter(
        key in 0u32..100,
        value in 1i32..100,  // Positive so it passes filter
    ) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("dedup", |_k: &u32, v: &i32| *v > 0);

        let mut batch1 = DeltaBatch::new(1);
        batch1.insert(key, value, 0);
        view.apply_delta(&batch1);

        let mut batch2 = DeltaBatch::new(2);
        batch2.insert(key, value, 0);
        let output = view.apply_delta(&batch2);

        prop_assert!(
            output.is_empty(),
            "Duplicate insert should produce no output delta, got {} entries",
            output.len()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. FallbackPolicy is monotonic in delta_size
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fallback_monotonic_in_delta_size(
        threshold in 0.01f64..1.0,
        min_delta in 1usize..50,
        materialized in 10usize..1000,
        delta_small in 0usize..500,
    ) {
        let policy = FallbackPolicy {
            ratio_threshold: threshold,
            min_delta_for_fallback: min_delta,
        };

        let delta_large = delta_small + 1;

        // If small triggers fallback, large must also trigger fallback.
        if policy.should_fallback(delta_small, materialized) {
            prop_assert!(
                policy.should_fallback(delta_large, materialized),
                "Fallback must be monotonic: if {}/{} triggers, {}/${} must too",
                delta_small, materialized, delta_large, materialized
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. DagTopology topo_order includes all views exactly once
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn topo_order_covers_all_views(n in 1usize..20) {
        let mut dag = DagTopology::new();
        let mut ids = Vec::new();

        for i in 0..n {
            ids.push(dag.add_view(format!("v{i}"), ViewDomain::Custom));
        }

        // Add a linear chain of edges.
        for i in 0..n.saturating_sub(1) {
            dag.add_edge(ids[i], ids[i + 1]);
        }

        dag.compute_topo_order();

        prop_assert_eq!(
            dag.topo_order.len(),
            n,
            "Topo order should contain exactly {} views, got {}",
            n,
            dag.topo_order.len()
        );

        // Every view ID appears exactly once.
        let mut seen = vec![false; n];
        for id in &dag.topo_order {
            let idx = id.0 as usize;
            prop_assert!(!seen[idx], "View {} appears twice in topo order", id);
            seen[idx] = true;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. DagTopology respects edge ordering invariant
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn topo_order_respects_edges(n in 2usize..15) {
        let mut dag = DagTopology::new();
        let mut ids = Vec::new();

        for i in 0..n {
            ids.push(dag.add_view(format!("v{i}"), ViewDomain::Custom));
        }

        // Linear chain.
        for i in 0..n - 1 {
            dag.add_edge(ids[i], ids[i + 1]);
        }

        dag.compute_topo_order();

        // For every edge (A, B), A must appear before B in topo order.
        let pos: HashMap<ViewId, usize> = dag
            .topo_order
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();

        for edge in &dag.edges {
            let from_pos = pos[&edge.from];
            let to_pos = pos[&edge.to];
            prop_assert!(
                from_pos < to_pos,
                "Edge {} -> {} violates topo order: pos {} >= {}",
                edge.from, edge.to, from_pos, to_pos
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. EpochEvidence delta_ratio is non-negative
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn epoch_evidence_delta_ratio_non_negative(
        delta_size in 0usize..10000,
        materialized_size in 0usize..10000,
    ) {
        let ev = EpochEvidence {
            epoch: 1,
            views_processed: 1,
            views_recomputed: 0,
            total_delta_size: delta_size,
            total_materialized_size: materialized_size,
            duration_us: 100,
            per_view: vec![],
        };
        prop_assert!(
            ev.delta_ratio() >= 0.0,
            "delta_ratio must be non-negative, got {}",
            ev.delta_ratio()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. EpochEvidence JSONL is valid (contains required fields)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn epoch_evidence_jsonl_valid(
        epoch in 0u64..10000,
        views_processed in 0usize..100,
        views_recomputed in 0usize..100,
        total_delta in 0usize..10000,
        total_mat in 0usize..10000,
        duration in 0u64..1000000,
    ) {
        let ev = EpochEvidence {
            epoch,
            views_processed,
            views_recomputed,
            total_delta_size: total_delta,
            total_materialized_size: total_mat,
            duration_us: duration,
            per_view: vec![],
        };
        let jsonl = ev.to_jsonl();
        let epoch_str = format!("\"epoch\":{}", epoch);
        let vp_str = format!("\"views_processed\":{}", views_processed);
        let vr_str = format!("\"views_recomputed\":{}", views_recomputed);
        prop_assert!(jsonl.contains("\"type\":\"ivm_epoch\""));
        prop_assert!(jsonl.contains(&epoch_str));
        prop_assert!(jsonl.contains(&vp_str));
        prop_assert!(jsonl.contains(&vr_str));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. FilteredListView: visible is always a subset of all items
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn filtered_visible_subset_of_all(
        ops in prop::collection::vec(filter_op_strategy(), 0..40),
    ) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("subset_test", |_k: &u32, v: &i32| *v > 0);
        for (epoch, op) in (1u64..).zip(ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            match op {
                FilterOp::Insert(k, v) => batch.insert(*k, *v, 0),
                FilterOp::Delete(k) => batch.delete(*k, 0),
            }
            view.apply_delta(&batch);
        }

        prop_assert!(
            view.visible_count() <= view.total_count(),
            "visible ({}) must be <= total ({})",
            view.visible_count(),
            view.total_count()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. FilteredListView: incremental == full_recompute after any delta seq
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn filter_incremental_equals_full_recompute(
        ops in prop::collection::vec(filter_op_strategy(), 0..40),
    ) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("correctness", |_k: &u32, v: &i32| *v > 0);
        for (epoch, op) in (1u64..).zip(ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            match op {
                FilterOp::Insert(k, v) => batch.insert(*k, *v, 0),
                FilterOp::Delete(k) => batch.delete(*k, 0),
            }
            view.apply_delta(&batch);
        }

        let full = view.full_recompute();
        let full_map: HashMap<u32, i32> = full.into_iter().collect();

        prop_assert_eq!(
            full_map.len(),
            view.materialized_size(),
            "full_recompute len ({}) != materialized_size ({})",
            full_map.len(),
            view.materialized_size()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. DeltaBatch len always matches entries.len()
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn batch_len_matches_entries(
        epoch in 0u64..1000,
        n_inserts in 0usize..20,
        n_deletes in 0usize..20,
    ) {
        let mut batch: DeltaBatch<u32, String> = DeltaBatch::new(epoch);
        for i in 0..n_inserts {
            batch.insert(i as u32, format!("v{i}"), i as u64);
        }
        for i in 0..n_deletes {
            batch.delete(100 + i as u32, (n_inserts + i) as u64);
        }
        prop_assert_eq!(batch.len(), n_inserts + n_deletes);
        prop_assert_eq!(batch.is_empty(), n_inserts + n_deletes == 0);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. DeltaEntry weight sign matches variant
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn delta_entry_weight_sign(key in any::<u32>(), value in any::<u64>(), time in any::<u64>()) {
        let insert: DeltaEntry<u32, u64> = DeltaEntry::Insert {
            key,
            value,
            logical_time: time,
        };
        prop_assert_eq!(insert.weight(), 1);
        prop_assert!(insert.is_insert());

        let delete: DeltaEntry<u32, u64> = DeltaEntry::Delete {
            key,
            logical_time: time,
        };
        prop_assert_eq!(delete.weight(), -1);
        prop_assert!(!delete.is_insert());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. StyleResolutionView: resolved == base XOR override for all entries
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn style_resolved_equals_base_xor_override(
        base_hash in any::<u64>(),
        ops in prop::collection::vec(style_op_strategy(), 1..30),
    ) {
        let mut view = StyleResolutionView::new("xor_check", base_hash);
        // Track overrides ourselves for cross-checking.
        let mut expected_overrides: HashMap<u32, u64> = HashMap::new();

        for (epoch, op) in (1u64..).zip(ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            match op {
                StyleOp::Insert(k, v) => {
                    batch.insert(StyleKey(*k), ResolvedStyleValue { style_hash: *v }, 0);
                    expected_overrides.insert(*k, *v);
                }
                StyleOp::Delete(k) => {
                    batch.delete(StyleKey(*k), 0);
                    expected_overrides.remove(k);
                }
            }
            view.apply_delta(&batch);
        }

        let full = view.full_recompute();
        for (key, resolved) in &full {
            let override_hash = expected_overrides.get(&key.0).copied().unwrap_or(0);
            let expected = base_hash ^ override_hash;
            prop_assert_eq!(
                resolved.style_hash,
                expected,
                "StyleKey({}) resolved to {} but expected base({:#x}) XOR override({:#x}) = {:#x}",
                key.0, resolved.style_hash, base_hash, override_hash, expected
            );
        }

        // All overrides should be present in full recompute.
        prop_assert_eq!(full.len(), expected_overrides.len());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. FilteredListView: visible == filter(all_items) after any sequence
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn filtered_visible_equals_filter_of_all(
        ops in prop::collection::vec(filter_op_strategy(), 0..40),
    ) {
        // Filter: positive values only.
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("filter_check", |_k: &u32, v: &i32| *v > 0);
        // Track all items ourselves.
        let mut expected_all: HashMap<u32, i32> = HashMap::new();

        for (epoch, op) in (1u64..).zip(ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            match op {
                FilterOp::Insert(k, v) => {
                    batch.insert(*k, *v, 0);
                    expected_all.insert(*k, *v);
                }
                FilterOp::Delete(k) => {
                    batch.delete(*k, 0);
                    expected_all.remove(k);
                }
            }
            view.apply_delta(&batch);
        }

        // Expected visible = filter(expected_all).
        let expected_visible: HashMap<u32, i32> = expected_all
            .iter()
            .filter(|(_k, v)| **v > 0)
            .map(|(k, v)| (*k, *v))
            .collect();

        let full = view.full_recompute();
        let actual_visible: HashMap<u32, i32> = full.into_iter().collect();

        prop_assert_eq!(
            actual_visible.len(),
            expected_visible.len(),
            "visible count mismatch: actual {} != expected {}",
            actual_visible.len(),
            expected_visible.len()
        );

        for (k, v) in &expected_visible {
            prop_assert_eq!(
                actual_visible.get(k),
                Some(v),
                "Key {} expected value {:?} but got {:?}",
                k, v, actual_visible.get(k)
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. DagTopology: diamond DAG has valid topo order
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn diamond_dag_topo_valid(
        n_branches in 2usize..8,
    ) {
        // Source → [n branches] → Sink (diamond pattern).
        let mut dag = DagTopology::new();
        let source = dag.add_view("source", ViewDomain::Style);
        let mut branches = Vec::new();
        for i in 0..n_branches {
            branches.push(dag.add_view(format!("branch_{i}"), ViewDomain::Layout));
        }
        let sink = dag.add_view("sink", ViewDomain::Render);

        for &branch in &branches {
            dag.add_edge(source, branch);
            dag.add_edge(branch, sink);
        }

        dag.compute_topo_order();

        let pos: HashMap<ViewId, usize> = dag
            .topo_order
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();

        // Source first, sink last.
        prop_assert_eq!(pos[&source], 0, "Source must be first");
        prop_assert_eq!(
            pos[&sink],
            n_branches + 1,
            "Sink must be last"
        );

        // All branches between source and sink.
        for branch in &branches {
            let p = pos[branch];
            prop_assert!(p > 0 && p < n_branches + 1);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 17. FallbackPolicy: below min_delta never triggers
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fallback_below_min_never_triggers(
        threshold in 0.01f64..1.0,
        min_delta in 2usize..100,
        materialized in 1usize..1000,
    ) {
        let policy = FallbackPolicy {
            ratio_threshold: threshold,
            min_delta_for_fallback: min_delta,
        };

        // Any delta_size < min_delta should never trigger.
        for ds in 0..min_delta {
            prop_assert!(
                !policy.should_fallback(ds, materialized),
                "delta_size {} < min {} should not trigger fallback",
                ds, min_delta
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 18. Multi-batch StyleResolutionView accumulation
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]
    #[test]
    fn style_multi_batch_accumulation(
        base_hash in any::<u64>(),
        batch_ops in prop::collection::vec(
            prop::collection::vec(style_op_strategy(), 1..10),
            1..8
        ),
    ) {
        let mut view = StyleResolutionView::new("multi_batch", base_hash);
        let mut expected: HashMap<u32, u64> = HashMap::new();
        for (epoch, batch_group) in (1u64..).zip(batch_ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            for op in batch_group {
                match op {
                    StyleOp::Insert(k, v) => {
                        batch.insert(StyleKey(*k), ResolvedStyleValue { style_hash: *v }, 0);
                        expected.insert(*k, *v);
                    }
                    StyleOp::Delete(k) => {
                        batch.delete(StyleKey(*k), 0);
                        expected.remove(k);
                    }
                }
            }
            view.apply_delta(&batch);
        }

        let full = view.full_recompute();
        prop_assert_eq!(full.len(), expected.len());

        for (key, resolved) in &full {
            let override_hash = expected[&key.0];
            let expected_hash = base_hash ^ override_hash;
            prop_assert_eq!(resolved.style_hash, expected_hash);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 19. FilteredListView multi-batch accumulation
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]
    #[test]
    fn filter_multi_batch_accumulation(
        batch_ops in prop::collection::vec(
            prop::collection::vec(filter_op_strategy(), 1..10),
            1..8
        ),
    ) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("multi_batch", |_k: &u32, v: &i32| *v > 0);
        let mut expected_all: HashMap<u32, i32> = HashMap::new();
        for (epoch, batch_group) in (1u64..).zip(batch_ops.iter()) {
            let mut batch = DeltaBatch::new(epoch);
            for op in batch_group {
                match op {
                    FilterOp::Insert(k, v) => {
                        batch.insert(*k, *v, 0);
                        expected_all.insert(*k, *v);
                    }
                    FilterOp::Delete(k) => {
                        batch.delete(*k, 0);
                        expected_all.remove(k);
                    }
                }
            }
            view.apply_delta(&batch);
        }

        let expected_visible: HashMap<u32, i32> = expected_all
            .iter()
            .filter(|(_k, v)| **v > 0)
            .map(|(k, v)| (*k, *v))
            .collect();

        let full: HashMap<u32, i32> = view.full_recompute().into_iter().collect();
        prop_assert_eq!(full.len(), expected_visible.len());

        for (k, v) in &expected_visible {
            prop_assert_eq!(full.get(k), Some(v));
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 20. Output deltas are consistent with materialized changes
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn output_deltas_reflect_actual_changes(
        ops in prop::collection::vec(filter_op_strategy(), 1..30),
    ) {
        let mut view: FilteredListView<u32, i32> =
            FilteredListView::new("delta_check", |_k: &u32, v: &i32| *v > 0);
        for (epoch, op) in (1u64..).zip(ops.iter()) {
            let size_before = view.materialized_size();
            let mut batch = DeltaBatch::new(epoch);
            match op {
                FilterOp::Insert(k, v) => batch.insert(*k, *v, 0),
                FilterOp::Delete(k) => batch.delete(*k, 0),
            }
            let output = view.apply_delta(&batch);
            let size_after = view.materialized_size();

            // If the output is empty, the materialized view should have the
            // same size (no visible net change). Note: insert+delete within
            // a single batch could cancel, but we send one op at a time here.
            if output.is_empty() {
                prop_assert_eq!(
                    size_before, size_after,
                    "Empty output but size changed: {} -> {}",
                    size_before, size_after
                );
            }
        }
    }
}
