//! Benchmarks for mermaid_layout (production Sugiyama layout + A* routing).
//!
//! Run with: cargo bench -p ftui-extras --bench mermaid_layout_bench --features diagram

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

#[cfg(feature = "diagram")]
use ftui_extras::mermaid::*;
#[cfg(feature = "diagram")]
use ftui_extras::mermaid_layout::{RoutingWeights, layout_diagram, route_all_edges};
#[cfg(feature = "diagram")]
use std::collections::BTreeMap;

#[cfg(feature = "diagram")]
fn dummy_span() -> Span {
    Span {
        start: Position {
            line: 1,
            col: 1,
            byte: 0,
        },
        end: Position {
            line: 1,
            col: 1,
            byte: 0,
        },
    }
}

#[cfg(feature = "diagram")]
fn make_ir(
    node_labels: &[&str],
    edges: &[(usize, usize)],
    direction: GraphDirection,
) -> MermaidDiagramIr {
    let nodes: Vec<IrNode> = node_labels
        .iter()
        .map(|label| IrNode {
            id: label.to_string(),
            label: None,
            shape: NodeShape::Rect,
            classes: Vec::new(),
            style_ref: None,
            span_primary: dummy_span(),
            span_all: Vec::new(),
            implicit: false,
            members: Vec::new(),
        })
        .collect();

    let ir_edges: Vec<IrEdge> = edges
        .iter()
        .map(|&(from, to)| IrEdge {
            from: IrEndpoint::Node(IrNodeId(from)),
            to: IrEndpoint::Node(IrNodeId(to)),
            arrow: "-->".to_string(),
            label: None,
            style_ref: None,
            span: dummy_span(),
        })
        .collect();

    MermaidDiagramIr {
        diagram_type: DiagramType::Graph,
        direction,
        nodes,
        edges: ir_edges,
        ports: Vec::new(),
        clusters: Vec::new(),
        labels: Vec::new(),
        pie_entries: Vec::new(),
        pie_title: None,
        pie_show_data: false,
        style_refs: Vec::new(),
        links: Vec::new(),
        meta: MermaidDiagramMeta {
            diagram_type: DiagramType::Graph,
            direction,
            support_level: MermaidSupportLevel::Supported,
            init: MermaidInitParse {
                config: MermaidInitConfig {
                    theme: None,
                    theme_variables: BTreeMap::new(),
                    flowchart_direction: None,
                },
                warnings: Vec::new(),
                errors: Vec::new(),
            },
            theme_overrides: MermaidThemeOverrides {
                theme: None,
                theme_variables: BTreeMap::new(),
            },
            guard: MermaidGuardReport::default(),
        },
        constraints: Vec::new(),
    }
}

#[cfg(feature = "diagram")]
fn default_config() -> MermaidConfig {
    MermaidConfig::default()
}

/// Build a linear chain: 0->1->2->...->n-1
#[cfg(feature = "diagram")]
fn linear_chain(n: usize) -> MermaidDiagramIr {
    let labels: Vec<String> = (0..n).map(|i| format!("N{i}")).collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let edges: Vec<(usize, usize)> = (0..n.saturating_sub(1)).map(|i| (i, i + 1)).collect();
    make_ir(&label_refs, &edges, GraphDirection::TB)
}

/// Build a diamond DAG with middle layers.
#[cfg(feature = "diagram")]
fn diamond_dag(n: usize) -> MermaidDiagramIr {
    let layers = 4;
    let per_layer = n.max(4) / layers;
    let total = 2 + per_layer * (layers - 2);
    let labels: Vec<String> = (0..total).map(|i| format!("N{i}")).collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

    let mut edges = Vec::new();
    for i in 1..=per_layer {
        edges.push((0, i));
    }
    for layer in 0..(layers - 3) {
        let start = 1 + layer * per_layer;
        let next_start = start + per_layer;
        for i in start..start + per_layer {
            for j in next_start..next_start + per_layer {
                edges.push((i, j));
            }
        }
    }
    let sink = total - 1;
    let last_start = 1 + (layers - 3) * per_layer;
    for i in last_start..last_start + per_layer {
        edges.push((i, sink));
    }

    make_ir(&label_refs, &edges, GraphDirection::TB)
}

/// Build a deterministic pseudo-random DAG with n nodes.
#[cfg(feature = "diagram")]
fn random_dag(n: usize) -> MermaidDiagramIr {
    let labels: Vec<String> = (0..n).map(|i| format!("N{i}")).collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

    let mut edges = Vec::new();
    let mut seed: u64 = 42;
    for i in 0..n.saturating_sub(1) {
        let fan_out = 1 + (seed % 3) as usize;
        for _ in 0..fan_out {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let target = i + 1 + (seed as usize % (n - i - 1).max(1));
            if target < n {
                edges.push((i, target));
            }
        }
    }

    make_ir(&label_refs, &edges, GraphDirection::TB)
}

#[cfg(feature = "diagram")]
fn bench_mermaid_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("mermaid_layout");
    let config = default_config();

    // Small: 5-node linear chain
    let small = linear_chain(5);
    let small_n = small.nodes.len() as u64;
    group.throughput(Throughput::Elements(small_n));
    group.bench_with_input(BenchmarkId::new("small", small_n), &small, |b, ir| {
        b.iter(|| black_box(layout_diagram(black_box(ir), black_box(&config))))
    });

    // Medium: diamond DAG
    let medium = diamond_dag(20);
    let medium_n = medium.nodes.len() as u64;
    group.throughput(Throughput::Elements(medium_n));
    group.bench_with_input(BenchmarkId::new("medium", medium_n), &medium, |b, ir| {
        b.iter(|| black_box(layout_diagram(black_box(ir), black_box(&config))))
    });

    // Large: random DAG
    let large = random_dag(100);
    let large_n = large.nodes.len() as u64;
    group.throughput(Throughput::Elements(large_n));
    group.bench_with_input(BenchmarkId::new("large", large_n), &large, |b, ir| {
        b.iter(|| black_box(layout_diagram(black_box(ir), black_box(&config))))
    });

    group.finish();
}

#[cfg(feature = "diagram")]
fn bench_astar_routing(c: &mut Criterion) {
    let mut group = c.benchmark_group("astar_routing");
    let config = default_config();
    let weights = RoutingWeights::default();

    // Medium: layout first, then benchmark routing
    let medium = diamond_dag(20);
    let medium_layout = layout_diagram(&medium, &config);
    group.bench_function("medium", |b| {
        b.iter(|| {
            black_box(route_all_edges(
                black_box(&medium),
                black_box(&medium_layout),
                black_box(&config),
                black_box(&weights),
            ))
        })
    });

    // Large: random DAG with A* routing
    let large = random_dag(50);
    let large_layout = layout_diagram(&large, &config);
    group.bench_function("large", |b| {
        b.iter(|| {
            black_box(route_all_edges(
                black_box(&large),
                black_box(&large_layout),
                black_box(&config),
                black_box(&weights),
            ))
        })
    });

    group.finish();
}

#[cfg(not(feature = "diagram"))]
fn bench_mermaid_layout(_c: &mut Criterion) {}
#[cfg(not(feature = "diagram"))]
fn bench_astar_routing(_c: &mut Criterion) {}

criterion_group!(benches, bench_mermaid_layout, bench_astar_routing);
criterion_main!(benches);
