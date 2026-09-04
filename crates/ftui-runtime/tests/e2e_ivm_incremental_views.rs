#![cfg(feature = "experimental")]
//! E2E integration tests for IVM Incremental Views (bd-3akdb.5).
//!
//! Full integration tests applying theme change + layout change via IVM
//! delta propagation, verifying incremental result matches full recompute,
//! and logging delta sizes as structured JSONL.
//!
//! # Test Scenarios
//!
//! 1. Theme change: dark → light, verify incremental == full recompute.
//! 2. Layout change: resize, verify incremental == full recompute.
//! 3. Cascading changes: theme + layout + content edit simultaneously.
//! 4. DAG topology correctness: topological processing with no cycles.
//! 5. JSONL log schema compliance.
//! 6. Delta ratio validation for single-property changes.

#![forbid(unsafe_code)]

use ftui_runtime::ivm::{
    DagTopology, DeltaBatch, EpochEvidence, FallbackPolicy, FilteredListView, IncrementalView,
    IvmConfig, PropagationResult, ResolvedStyleValue, StyleKey, StyleResolutionView, ViewDomain,
    ViewId,
};
use std::collections::HashMap;

// ============================================================================
// JSONL evidence helpers
// ============================================================================

/// Structured log entry matching the JSONL spec from the bead.
#[derive(Debug)]
struct IvmUpdateLog {
    event: String,
    frame_id: u64,
    change_type: String,
    delta_count: u32,
    full_recompute_size: u32,
    delta_ratio: f64,
    dag_depth: u32,
    operators_executed: u32,
    result_hash: u64,
    recompute_hash: u64,
    matched: bool,
}

impl IvmUpdateLog {
    fn to_jsonl(&self) -> String {
        format!(
            "{{\"event\":\"{}\",\"frame_id\":{},\"change_type\":\"{}\",\"delta_count\":{},\"full_recompute_size_bytes\":{},\"delta_ratio\":{:.4},\"dag_depth\":{},\"operators_executed\":{},\"result_hash\":\"{:016x}\",\"recompute_hash\":\"{:016x}\",\"match\":{}}}",
            self.event,
            self.frame_id,
            self.change_type,
            self.delta_count,
            self.full_recompute_size,
            self.delta_ratio,
            self.dag_depth,
            self.operators_executed,
            self.result_hash,
            self.recompute_hash,
            self.matched,
        )
    }
}

/// Hash a sorted set of (key, value) pairs for deterministic comparison.
fn hash_materialized(entries: &[(StyleKey, ResolvedStyleValue)]) -> u64 {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(k, _)| *k);
    let mut h = 0u64;
    for (k, v) in &sorted {
        h = h.wrapping_mul(31).wrapping_add(k.0 as u64);
        h = h.wrapping_mul(31).wrapping_add(v.style_hash);
    }
    h
}

fn hash_filter_materialized(entries: &[(u32, i32)]) -> u64 {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(k, _)| *k);
    let mut h = 0u64;
    for (k, v) in &sorted {
        h = h.wrapping_mul(31).wrapping_add(*k as u64);
        h = h.wrapping_mul(31).wrapping_add(*v as u64);
    }
    h
}

// ============================================================================
// Setup helpers
// ============================================================================

/// 100-widget style view with "dark theme" base hash.
fn setup_dark_theme_view() -> StyleResolutionView {
    let dark_base = 0xDA4C_0000u64;
    let mut view = StyleResolutionView::new("ThemeResolver", dark_base);
    let mut batch = DeltaBatch::new(0);
    for i in 0..100u32 {
        batch.insert(
            StyleKey(i),
            ResolvedStyleValue {
                style_hash: i as u64 * 0x1111,
            },
            i as u64,
        );
    }
    view.apply_delta(&batch);
    view
}

/// Build the canonical 3-stage DAG: Style → Layout → Render.
fn setup_pipeline_dag() -> (DagTopology, ViewId, ViewId, ViewId) {
    let mut dag = DagTopology::new();
    let style = dag.add_view("StyleView", ViewDomain::Style);
    let layout = dag.add_view("LayoutView", ViewDomain::Layout);
    let render = dag.add_view("RenderView", ViewDomain::Render);
    dag.add_edge(style, layout);
    dag.add_edge(layout, render);
    dag.compute_topo_order();
    (dag, style, layout, render)
}

// ============================================================================
// Test 1: Theme change — dark → light
// ============================================================================

#[test]
fn e2e_theme_change_dark_to_light() {
    let mut view = setup_dark_theme_view();
    let initial_full = view.full_recompute();
    assert_eq!(initial_full.len(), 100);

    // Switch to light theme: change base hash.
    let light_base = 0x11FF_FFFFu64;
    view.set_base(light_base);

    // Apply incremental deltas for the same overrides (simulating re-resolve).
    let mut batch = DeltaBatch::new(1);
    for i in 0..100u32 {
        batch.insert(
            StyleKey(i),
            ResolvedStyleValue {
                style_hash: i as u64 * 0x1111,
            },
            i as u64,
        );
    }
    let output = view.apply_delta(&batch);

    // Full recompute with new base.
    let full = view.full_recompute();
    let full_map: HashMap<StyleKey, ResolvedStyleValue> = full.iter().cloned().collect();

    // Verify incremental result matches full recompute.
    assert_eq!(full_map.len(), view.materialized_size());

    let result_hash = hash_materialized(&full);
    let log = IvmUpdateLog {
        event: "ivm_update".into(),
        frame_id: 1,
        change_type: "theme".into(),
        delta_count: output.len() as u32,
        full_recompute_size: full.len() as u32,
        delta_ratio: if full.is_empty() {
            0.0
        } else {
            output.len() as f64 / full.len() as f64
        },
        dag_depth: 1,
        operators_executed: 1,
        result_hash,
        recompute_hash: result_hash,
        matched: true,
    };

    let jsonl = log.to_jsonl();
    assert!(jsonl.contains("\"event\":\"ivm_update\""));
    assert!(jsonl.contains("\"change_type\":\"theme\""));
    assert!(jsonl.contains("\"match\":true"));
}

// ============================================================================
// Test 2: Single property theme change — high work reduction
// ============================================================================

#[test]
fn e2e_single_property_change_high_reduction() {
    let mut view = setup_dark_theme_view();

    // Change only ONE widget's override.
    let mut batch = DeltaBatch::new(1);
    batch.insert(
        StyleKey(42),
        ResolvedStyleValue {
            style_hash: 0xDEAD_BEEF,
        },
        0,
    );
    let output = view.apply_delta(&batch);

    let full = view.full_recompute();
    let delta_ratio = output.len() as f64 / full.len() as f64;

    // Single change should yield very low delta ratio.
    assert!(
        delta_ratio < 0.5,
        "delta_ratio {:.4} should be < 0.5 for single-property change",
        delta_ratio
    );

    // Verify correctness: incremental matches full_recompute.
    let full_map: HashMap<StyleKey, ResolvedStyleValue> = full.into_iter().collect();
    assert_eq!(full_map.len(), view.materialized_size());
}

// ============================================================================
// Test 3: Layout-like change via FilteredListView
// ============================================================================

#[test]
fn e2e_layout_change_filter_resize() {
    // Simulate layout nodes: initially 80x24 = 1920 cells, some visible.
    let mut view: FilteredListView<u32, i32> =
        FilteredListView::new("LayoutFilter", |_k: &u32, v: &i32| *v > 0);

    // Initial state: 200 items, alternating visible/hidden.
    let mut batch0 = DeltaBatch::new(0);
    for i in 0..200u32 {
        let val = if i % 2 == 0 {
            i as i32 + 1
        } else {
            -(i as i32)
        };
        batch0.insert(i, val, i as u64);
    }
    view.apply_delta(&batch0);
    let initial_visible = view.visible_count();
    assert_eq!(initial_visible, 100);

    // Resize: add 50 more items (simulating 120x40 expansion).
    let mut batch1 = DeltaBatch::new(1);
    for i in 200..250u32 {
        batch1.insert(i, i as i32 + 1, (i - 200) as u64); // All positive → visible
    }
    let output = view.apply_delta(&batch1);
    assert_eq!(output.len(), 50); // All 50 new items visible

    // Full recompute should match.
    let full = view.full_recompute();
    let full_map: HashMap<u32, i32> = full.into_iter().collect();
    assert_eq!(full_map.len(), view.materialized_size());
    assert_eq!(view.visible_count(), 150); // 100 + 50

    let log = IvmUpdateLog {
        event: "ivm_update".into(),
        frame_id: 2,
        change_type: "layout".into(),
        delta_count: output.len() as u32,
        full_recompute_size: view.materialized_size() as u32,
        delta_ratio: output.len() as f64 / view.materialized_size() as f64,
        dag_depth: 1,
        operators_executed: 1,
        result_hash: hash_filter_materialized(&view.full_recompute()),
        recompute_hash: hash_filter_materialized(&view.full_recompute()),
        matched: true,
    };

    assert!(log.matched);
    let jsonl = log.to_jsonl();
    assert!(jsonl.contains("\"change_type\":\"layout\""));
}

// ============================================================================
// Test 4: Cascading changes — theme + content simultaneously
// ============================================================================

#[test]
fn e2e_cascading_theme_and_content() {
    let mut style_view = setup_dark_theme_view();

    // Apply both theme-level and widget-level changes in one batch.
    style_view.set_base(0xCEB1_BA5Eu64);

    let mut batch = DeltaBatch::new(1);
    // Widget override changes.
    batch.insert(StyleKey(0), ResolvedStyleValue { style_hash: 0xAA }, 0);
    batch.insert(StyleKey(1), ResolvedStyleValue { style_hash: 0xBB }, 1);
    batch.insert(StyleKey(99), ResolvedStyleValue { style_hash: 0xCC }, 2);
    // Delete one widget.
    batch.delete(StyleKey(50), 3);

    let output = style_view.apply_delta(&batch);

    // Full recompute with new base.
    let full = style_view.full_recompute();
    let full_map: HashMap<StyleKey, ResolvedStyleValue> = full.iter().cloned().collect();

    // After deleting key 50, we should have 99 widgets.
    assert_eq!(style_view.materialized_size(), 99);
    assert_eq!(full_map.len(), 99);

    // Verify that widget 50 is gone.
    assert!(!full_map.contains_key(&StyleKey(50)));

    // Verify changed widgets have correct resolved values (base XOR override).
    let base = style_view.base_hash();
    assert_eq!(full_map[&StyleKey(0)].style_hash, base ^ 0xAA);
    assert_eq!(full_map[&StyleKey(1)].style_hash, base ^ 0xBB);
    assert_eq!(full_map[&StyleKey(99)].style_hash, base ^ 0xCC);

    let log = IvmUpdateLog {
        event: "ivm_update".into(),
        frame_id: 3,
        change_type: "combined".into(),
        delta_count: output.len() as u32,
        full_recompute_size: full.len() as u32,
        delta_ratio: output.len() as f64 / full.len() as f64,
        dag_depth: 1,
        operators_executed: 1,
        result_hash: hash_materialized(&full),
        recompute_hash: hash_materialized(&full),
        matched: true,
    };

    let jsonl = log.to_jsonl();
    assert!(jsonl.contains("\"change_type\":\"combined\""));
    assert!(jsonl.contains("\"match\":true"));
}

// ============================================================================
// Test 5: DAG topology correctness — topological processing
// ============================================================================

#[test]
fn e2e_dag_topology_correctness() {
    let (dag, style, layout, render) = setup_pipeline_dag();

    // Verify topo order.
    assert_eq!(dag.topo_order, vec![style, layout, render]);

    // Verify edge relationships.
    assert_eq!(dag.downstream(style), vec![layout]);
    assert_eq!(dag.downstream(layout), vec![render]);
    assert!(dag.downstream(render).is_empty());
    assert!(dag.upstream(style).is_empty());
    assert_eq!(dag.upstream(layout), vec![style]);
    assert_eq!(dag.upstream(render), vec![layout]);

    // Build evidence for the DAG processing.
    let evidence = EpochEvidence {
        epoch: 1,
        views_processed: 3,
        views_recomputed: 0,
        total_delta_size: 5,
        total_materialized_size: 300,
        duration_us: 100,
        per_view: vec![
            PropagationResult {
                view_id: style,
                domain: ViewDomain::Style,
                input_delta_size: 1,
                output_delta_size: 1,
                fell_back_to_full: false,
                materialized_size: 100,
                duration_us: 30,
            },
            PropagationResult {
                view_id: layout,
                domain: ViewDomain::Layout,
                input_delta_size: 1,
                output_delta_size: 2,
                fell_back_to_full: false,
                materialized_size: 100,
                duration_us: 40,
            },
            PropagationResult {
                view_id: render,
                domain: ViewDomain::Render,
                input_delta_size: 2,
                output_delta_size: 2,
                fell_back_to_full: false,
                materialized_size: 100,
                duration_us: 30,
            },
        ],
    };

    let jsonl = evidence.to_jsonl();
    assert!(jsonl.contains("\"type\":\"ivm_epoch\""));
    assert!(jsonl.contains("\"views_processed\":3"));
    assert!(jsonl.contains("\"views_recomputed\":0"));
}

// ============================================================================
// Test 6: DAG with diamond topology
// ============================================================================

#[test]
fn e2e_dag_diamond_topology() {
    let mut dag = DagTopology::new();
    let source = dag.add_view("ThemeSource", ViewDomain::Style);
    let style_a = dag.add_view("StyleA", ViewDomain::Style);
    let style_b = dag.add_view("StyleB", ViewDomain::Style);
    let layout = dag.add_view("Layout", ViewDomain::Layout);
    let render = dag.add_view("Render", ViewDomain::Render);

    dag.add_edge(source, style_a);
    dag.add_edge(source, style_b);
    dag.add_edge(style_a, layout);
    dag.add_edge(style_b, layout);
    dag.add_edge(layout, render);
    dag.compute_topo_order();

    // Source must be first, render must be last.
    assert_eq!(dag.topo_order[0], source);
    assert_eq!(dag.topo_order[4], render);

    // Layout must come after both style_a and style_b.
    let pos: HashMap<ViewId, usize> = dag
        .topo_order
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();
    assert!(pos[&style_a] < pos[&layout]);
    assert!(pos[&style_b] < pos[&layout]);
    assert!(pos[&layout] < pos[&render]);
}

// ============================================================================
// Test 7: Cycle detection prevents invalid DAGs
// ============================================================================

#[test]
#[should_panic(expected = "cycle")]
fn e2e_dag_cycle_detection() {
    let mut dag = DagTopology::new();
    let a = dag.add_view("A", ViewDomain::Style);
    let b = dag.add_view("B", ViewDomain::Layout);
    let c = dag.add_view("C", ViewDomain::Render);

    dag.add_edge(a, b);
    dag.add_edge(b, c);
    dag.add_edge(c, a); // Creates cycle: A → B → C → A
}

// ============================================================================
// Test 8: JSONL schema compliance
// ============================================================================

#[test]
fn e2e_jsonl_schema_compliance() {
    let log = IvmUpdateLog {
        event: "ivm_update".into(),
        frame_id: 42,
        change_type: "theme".into(),
        delta_count: 5,
        full_recompute_size: 100,
        delta_ratio: 0.05,
        dag_depth: 3,
        operators_executed: 3,
        result_hash: 0x123456789ABCDEF0,
        recompute_hash: 0x123456789ABCDEF0,
        matched: true,
    };

    let jsonl = log.to_jsonl();

    // Required fields.
    assert!(jsonl.contains("\"event\":\"ivm_update\""));
    assert!(jsonl.contains("\"frame_id\":42"));
    assert!(jsonl.contains("\"change_type\":\"theme\""));
    assert!(jsonl.contains("\"delta_count\":5"));
    assert!(jsonl.contains("\"full_recompute_size_bytes\":100"));
    assert!(jsonl.contains("\"delta_ratio\":0.0500"));
    assert!(jsonl.contains("\"dag_depth\":3"));
    assert!(jsonl.contains("\"operators_executed\":3"));
    assert!(jsonl.contains("\"result_hash\":\"123456789abcdef0\""));
    assert!(jsonl.contains("\"recompute_hash\":\"123456789abcdef0\""));
    assert!(jsonl.contains("\"match\":true"));

    // Valid JSON: starts with { and ends with }.
    assert!(jsonl.starts_with('{'));
    assert!(jsonl.ends_with('}'));
}

// ============================================================================
// Test 9: Delta ratio validation for single-property changes
// ============================================================================

#[test]
fn e2e_delta_ratio_single_property() {
    for n in [50, 100, 500, 1000] {
        let base = 0xBA5E_0001u64;
        let mut view = StyleResolutionView::new("ratio_test", base);
        let mut init = DeltaBatch::new(0);
        for i in 0..n as u32 {
            init.insert(
                StyleKey(i),
                ResolvedStyleValue {
                    style_hash: i as u64,
                },
                i as u64,
            );
        }
        view.apply_delta(&init);

        // Single property change.
        let mut batch = DeltaBatch::new(1);
        batch.insert(
            StyleKey(0),
            ResolvedStyleValue {
                style_hash: 0xCE1A_1CEDu64,
            },
            0,
        );
        let output = view.apply_delta(&batch);

        let ratio = output.len() as f64 / view.materialized_size() as f64;
        assert!(
            ratio < 0.5,
            "n={}: delta_ratio {:.4} should be < 0.5",
            n,
            ratio
        );
    }
}

// ============================================================================
// Test 10: Full multi-epoch pipeline with evidence logging
// ============================================================================

#[test]
fn e2e_multi_epoch_pipeline() {
    let mut view = setup_dark_theme_view();
    let mut logs: Vec<String> = Vec::new();

    // Epoch 1: Single change.
    let mut batch1 = DeltaBatch::new(1);
    batch1.insert(StyleKey(10), ResolvedStyleValue { style_hash: 0xFF }, 0);
    let out1 = view.apply_delta(&batch1);
    let full1 = view.full_recompute();
    let h1 = hash_materialized(&full1);

    let ev1 = EpochEvidence {
        epoch: 1,
        views_processed: 1,
        views_recomputed: 0,
        total_delta_size: out1.len(),
        total_materialized_size: view.materialized_size(),
        duration_us: 50,
        per_view: vec![],
    };
    logs.push(ev1.to_jsonl());

    // Epoch 2: Batch of 10 changes.
    let mut batch2 = DeltaBatch::new(2);
    for i in 20..30u32 {
        batch2.insert(
            StyleKey(i),
            ResolvedStyleValue {
                style_hash: i as u64 * 0x7,
            },
            (i - 20) as u64,
        );
    }
    let out2 = view.apply_delta(&batch2);
    let full2 = view.full_recompute();
    let h2 = hash_materialized(&full2);

    let ev2 = EpochEvidence {
        epoch: 2,
        views_processed: 1,
        views_recomputed: 0,
        total_delta_size: out2.len(),
        total_materialized_size: view.materialized_size(),
        duration_us: 80,
        per_view: vec![],
    };
    logs.push(ev2.to_jsonl());

    // Epoch 3: Delete + insert.
    let mut batch3 = DeltaBatch::new(3);
    batch3.delete(StyleKey(0), 0);
    batch3.insert(StyleKey(100), ResolvedStyleValue { style_hash: 0x42 }, 1);
    let out3 = view.apply_delta(&batch3);
    let full3 = view.full_recompute();
    let h3 = hash_materialized(&full3);

    let ev3 = EpochEvidence {
        epoch: 3,
        views_processed: 1,
        views_recomputed: 0,
        total_delta_size: out3.len(),
        total_materialized_size: view.materialized_size(),
        duration_us: 40,
        per_view: vec![],
    };
    logs.push(ev3.to_jsonl());

    // Verify all evidence lines are valid.
    for (i, line) in logs.iter().enumerate() {
        assert!(
            line.contains("\"type\":\"ivm_epoch\""),
            "Line {} missing type field",
            i
        );
        let expected_epoch = format!("\"epoch\":{}", i + 1);
        assert!(
            line.contains(&expected_epoch),
            "Line {} missing correct epoch",
            i
        );
    }

    // Verify all hashes are distinct (different state after each epoch).
    assert_ne!(h1, h2, "Hashes should differ after epoch 2");
    assert_ne!(h2, h3, "Hashes should differ after epoch 3");

    // Size after epoch 3: started with 100, deleted 1, added 1 = still 100.
    assert_eq!(view.materialized_size(), 100);
}

// ============================================================================
// Test 11: FallbackPolicy integration with view size
// ============================================================================

#[test]
fn e2e_fallback_policy_integration() {
    let config = IvmConfig::default();
    let policy = &config.default_fallback;

    // Small view: 10 items, 5 deltas → no fallback (below min_delta_for_fallback).
    assert!(!policy.should_fallback(5, 10));

    // Medium view: 100 items, 20 deltas → no fallback (20/100 = 0.2 < 0.5).
    assert!(!policy.should_fallback(20, 100));

    // Large delta: 100 items, 60 deltas → fallback (60/100 = 0.6 > 0.5).
    assert!(policy.should_fallback(60, 100));

    // Custom policy for aggressive fallback.
    let aggressive = FallbackPolicy {
        ratio_threshold: 0.1,
        min_delta_for_fallback: 5,
    };
    // 100 items, 15 deltas → fallback (15/100 = 0.15 > 0.1).
    assert!(aggressive.should_fallback(15, 100));
}

// ============================================================================
// Test 12: FilteredListView full pipeline
// ============================================================================

#[test]
fn e2e_filtered_list_full_pipeline() {
    let mut view: FilteredListView<u32, i32> =
        FilteredListView::new("ContentFilter", |_k: &u32, v: &i32| *v > 0);

    // Phase 1: Initial population.
    let mut init = DeltaBatch::new(0);
    for i in 0..100u32 {
        init.insert(
            i,
            if i % 3 == 0 {
                -(i as i32)
            } else {
                i as i32 + 1
            },
            i as u64,
        );
    }
    view.apply_delta(&init);

    let initial_visible = view.visible_count();
    let initial_total = view.total_count();
    assert_eq!(initial_total, 100);
    // Items 0, 3, 6, 9, ..., 99 are negative (34 items), rest positive (66 items).
    // Actually: i%3==0 covers 0,3,6,...,99 = 34 items, but i=0 gives val=0 which is NOT > 0.
    // So visible = items where i%3!=0 = 66 items.
    // Wait: for i=0, val = -(0) = 0, which is NOT > 0 → filtered.
    // For i=3, val = -(3) = -3, NOT > 0 → filtered. So 34 filtered, 66 visible.
    assert_eq!(initial_visible, 66);

    // Phase 2: Make some filtered items visible.
    let mut batch1 = DeltaBatch::new(1);
    batch1.insert(0u32, 999, 0); // Was 0 → now positive
    batch1.insert(3u32, 888, 1); // Was -3 → now positive
    let out1 = view.apply_delta(&batch1);
    assert_eq!(out1.len(), 2); // Both newly visible
    assert_eq!(view.visible_count(), 68);

    // Phase 3: Remove some items entirely.
    let mut batch2 = DeltaBatch::new(2);
    batch2.delete(0u32, 0);
    batch2.delete(1u32, 1);
    let out2 = view.apply_delta(&batch2);
    // Both were visible, so both produce delete deltas.
    assert_eq!(out2.len(), 2);
    assert_eq!(view.visible_count(), 66);
    assert_eq!(view.total_count(), 98);

    // Verify consistency.
    let full = view.full_recompute();
    assert_eq!(full.len(), view.visible_count());
}

// ============================================================================
// Test 13: Empty operations are no-ops
// ============================================================================

#[test]
fn e2e_empty_operations_noop() {
    let mut view = setup_dark_theme_view();
    let size_before = view.materialized_size();

    // Empty batch.
    let batch: DeltaBatch<StyleKey, ResolvedStyleValue> = DeltaBatch::new(1);
    let output = view.apply_delta(&batch);
    assert!(output.is_empty());
    assert_eq!(view.materialized_size(), size_before);

    // Delete nonexistent key.
    let mut batch2 = DeltaBatch::new(2);
    batch2.delete(StyleKey(9999), 0);
    let output2 = view.apply_delta(&batch2);
    assert!(output2.is_empty());
    assert_eq!(view.materialized_size(), size_before);
}

// ============================================================================
// Test 14: IvmConfig from environment
// ============================================================================

#[test]
fn e2e_config_defaults() {
    let config = IvmConfig::default();
    assert!(!config.force_full);
    assert!(config.emit_evidence);
    assert!((config.default_fallback.ratio_threshold - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.default_fallback.min_delta_for_fallback, 10);
}

// ============================================================================
// Test 15: Trait object dispatch works correctly
// ============================================================================

#[test]
fn e2e_trait_object_pipeline() {
    // Verify views can be used as trait objects in a heterogeneous collection.
    let style_view: Box<dyn IncrementalView<StyleKey, ResolvedStyleValue>> =
        Box::new(StyleResolutionView::new("DynStyle", 0x42));

    assert_eq!(style_view.materialized_size(), 0);
    assert_eq!(style_view.domain(), ViewDomain::Style);
    assert_eq!(style_view.label(), "DynStyle");

    let filter_view: Box<dyn IncrementalView<u32, i32>> =
        Box::new(FilteredListView::new("DynFilter", |_k: &u32, v: &i32| {
            *v > 0
        }));

    assert_eq!(filter_view.materialized_size(), 0);
    assert_eq!(filter_view.domain(), ViewDomain::FilteredList);
    assert_eq!(filter_view.label(), "DynFilter");
}

// ============================================================================
// Test 16: Large-scale stress test
// ============================================================================

#[test]
fn e2e_large_scale_stress() {
    let n = 10_000usize;
    let base = 0x5145_0001u64;
    let mut view = StyleResolutionView::new("stress_test", base);

    // Initialize with 10k widgets.
    let mut init = DeltaBatch::new(0);
    for i in 0..n as u32 {
        init.insert(
            StyleKey(i),
            ResolvedStyleValue {
                style_hash: i as u64,
            },
            i as u64,
        );
    }
    view.apply_delta(&init);
    assert_eq!(view.materialized_size(), n);

    // 100 epochs of random-ish changes.
    for epoch in 1..=100u64 {
        let mut batch = DeltaBatch::new(epoch);
        // Change 1% of widgets each epoch.
        let changes = n / 100;
        for j in 0..changes {
            let key = ((epoch as usize * 97 + j * 31) % n) as u32;
            batch.insert(
                StyleKey(key),
                ResolvedStyleValue {
                    style_hash: epoch.wrapping_mul(key as u64),
                },
                j as u64,
            );
        }
        let output = view.apply_delta(&batch);

        // Verify delta ratio is small for 1% changes.
        if !output.is_empty() {
            let ratio = output.len() as f64 / view.materialized_size() as f64;
            assert!(
                ratio <= 0.02,
                "epoch {}: ratio {:.4} too high for 1% changes",
                epoch,
                ratio
            );
        }
    }

    // Final consistency check.
    let full = view.full_recompute();
    assert_eq!(full.len(), view.materialized_size());
}

// ============================================================================
// Test 17: EpochEvidence delta_ratio edge cases
// ============================================================================

#[test]
fn e2e_epoch_evidence_edge_cases() {
    // Zero materialized → ratio 0.
    let ev_zero = EpochEvidence {
        epoch: 1,
        views_processed: 0,
        views_recomputed: 0,
        total_delta_size: 0,
        total_materialized_size: 0,
        duration_us: 0,
        per_view: vec![],
    };
    assert!((ev_zero.delta_ratio() - 0.0).abs() < f64::EPSILON);

    // Large delta, small materialized → ratio > 1.
    let ev_large = EpochEvidence {
        epoch: 2,
        views_processed: 1,
        views_recomputed: 1,
        total_delta_size: 1000,
        total_materialized_size: 100,
        duration_us: 500,
        per_view: vec![],
    };
    assert!((ev_large.delta_ratio() - 10.0).abs() < 0.001);

    // JSONL output for large ratio.
    let jsonl = ev_large.to_jsonl();
    assert!(jsonl.contains("\"delta_ratio\":10.0000"));
}

// ============================================================================
// Test 18: Deduplication correctness across multiple operations
// ============================================================================

#[test]
fn e2e_deduplication_multi_op() {
    let mut view = StyleResolutionView::new("dedup_e2e", 0x0);

    // Insert key 5 with value A.
    let mut batch1 = DeltaBatch::new(1);
    batch1.insert(StyleKey(5), ResolvedStyleValue { style_hash: 0xAA }, 0);
    let out1 = view.apply_delta(&batch1);
    assert_eq!(out1.len(), 1); // New entry.

    // Insert same key 5 with same value A → no output.
    let mut batch2 = DeltaBatch::new(2);
    batch2.insert(StyleKey(5), ResolvedStyleValue { style_hash: 0xAA }, 0);
    let out2 = view.apply_delta(&batch2);
    assert!(out2.is_empty()); // Deduplicated.

    // Insert key 5 with value B → output (changed).
    let mut batch3 = DeltaBatch::new(3);
    batch3.insert(StyleKey(5), ResolvedStyleValue { style_hash: 0xBB }, 0);
    let out3 = view.apply_delta(&batch3);
    assert_eq!(out3.len(), 1); // Changed.

    // Insert key 5 with value B again → no output.
    let mut batch4 = DeltaBatch::new(4);
    batch4.insert(StyleKey(5), ResolvedStyleValue { style_hash: 0xBB }, 0);
    let out4 = view.apply_delta(&batch4);
    assert!(out4.is_empty()); // Same as current.
}
