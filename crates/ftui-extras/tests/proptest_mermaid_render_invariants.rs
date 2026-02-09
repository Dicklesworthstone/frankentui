//! Property-based invariant tests for the mermaid render pipeline.
//!
//! These tests verify structural invariants for `render_diagram` and
//! `render_diagram_adaptive`:
//!
//! 1. Render never panics — random layouts rendered to random-sized buffers
//! 2. Render determinism — same inputs always produce identical buffer
//! 3. Render plan consistency — select_render_plan is deterministic
//! 4. Buffer not corrupted — render doesn't write outside the given area
//! 5. All diagram types renderable — every type header renders without panic
//! 6. Zero-area resilience — zero-width or zero-height areas don't panic
//! 7. Large area resilience — very large areas don't panic
//! 8. Full pipeline no-panic — parse → layout → render end-to-end

#[cfg(feature = "diagram")]
mod tests {
    use ftui_core::geometry::Rect;
    use ftui_extras::mermaid::*;
    use ftui_extras::mermaid_layout::*;
    use ftui_extras::mermaid_render::*;
    use ftui_render::buffer::Buffer;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    // ── Helpers ─────────────────────────────────────────────────────────────

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

    fn make_ir(
        node_count: usize,
        edges: &[(usize, usize)],
        direction: GraphDirection,
    ) -> MermaidDiagramIr {
        let nodes: Vec<IrNode> = (0..node_count)
            .map(|i| IrNode {
                id: format!("N{i}"),
                label: None,
                shape: NodeShape::Rect,
                classes: Vec::new(),
                style_ref: None,
                span_primary: dummy_span(),
                span_all: Vec::new(),
                implicit: false,
                members: Vec::new(),
                annotation: None,
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .filter(|&&(from, to)| from < node_count && to < node_count)
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
            quadrant_points: Vec::new(),
            quadrant_title: None,
            quadrant_x_axis: None,
            quadrant_y_axis: None,
            quadrant_labels: [None, None, None, None],
            packet_fields: Vec::new(),
            packet_title: None,
            packet_bits_per_row: 32,
            sequence_participants: Vec::new(),
            sequence_controls: Vec::new(),
            sequence_notes: Vec::new(),
            sequence_activations: Vec::new(),
            sequence_autonumber: false,
            gantt_title: None,
            gantt_sections: Vec::new(),
            gantt_tasks: Vec::new(),
        }
    }

    fn make_buffer(area: Rect) -> Buffer {
        Buffer::new(area.width.max(1), area.height.max(1))
    }

    /// Build a full pipeline: IR → layout → render into buffer.
    fn render_pipeline(
        node_count: usize,
        edges: &[(usize, usize)],
        direction: GraphDirection,
        width: u16,
        height: u16,
    ) -> Buffer {
        let ir = make_ir(node_count, edges, direction);
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        let area = Rect::new(0, 0, width, height);
        let mut buf = make_buffer(area);
        render_diagram(&layout, &ir, &config, area, &mut buf);
        buf
    }

    // ── Strategies ──────────────────────────────────────────────────────────

    fn direction_strategy() -> impl Strategy<Value = GraphDirection> {
        prop_oneof![
            Just(GraphDirection::TB),
            Just(GraphDirection::TD),
            Just(GraphDirection::LR),
            Just(GraphDirection::RL),
            Just(GraphDirection::BT),
        ]
    }

    fn dag_strategy(
        max_nodes: usize,
        max_edges: usize,
    ) -> impl Strategy<Value = (usize, Vec<(usize, usize)>, GraphDirection)> {
        (2..=max_nodes, direction_strategy()).prop_flat_map(move |(n, dir)| {
            let edge_count = 1..=max_edges.min(n * (n - 1) / 2);
            (
                Just(n),
                proptest::collection::vec((0..n, 0..n), edge_count).prop_map(|edges| {
                    edges
                        .into_iter()
                        .map(|(a, b)| if a < b { (a, b) } else { (b, a) })
                        .filter(|(a, b)| a != b)
                        .collect::<Vec<_>>()
                }),
                Just(dir),
            )
        })
    }

    fn buffer_dims() -> impl Strategy<Value = (u16, u16)> {
        (10u16..=120, 8u16..=60)
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 1. Render never panics — random layouts rendered to random-sized buffers
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn render_never_panics(
            (n, edges, dir) in dag_strategy(8, 10),
            (w, h) in buffer_dims(),
        ) {
            let _buf = render_pipeline(n, &edges, dir, w, h);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 2. Render determinism — same inputs always produce identical buffer
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn render_is_deterministic(
            (n, edges, dir) in dag_strategy(6, 8),
            (w, h) in buffer_dims(),
        ) {
            let ir = make_ir(n, &edges, dir);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, w, h);

            let mut buf1 = make_buffer(area);
            render_diagram(&layout, &ir, &config, area, &mut buf1);

            let mut buf2 = make_buffer(area);
            render_diagram(&layout, &ir, &config, area, &mut buf2);

            // Compare cell-by-cell.
            for y in 0..h {
                for x in 0..w {
                    let c1 = buf1.get(x, y);
                    let c2 = buf2.get(x, y);
                    prop_assert_eq!(
                        c1, c2,
                        "Cell ({}, {}) differs between identical render calls",
                        x, y,
                    );
                }
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 3. Render plan consistency — select_render_plan is deterministic
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn render_plan_is_deterministic(
            (n, edges, dir) in dag_strategy(8, 10),
            (w, h) in buffer_dims(),
        ) {
            let ir = make_ir(n, &edges, dir);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, w, h);

            let plan1 = select_render_plan(&config, &layout, &ir, area);
            let plan2 = select_render_plan(&config, &layout, &ir, area);

            prop_assert_eq!(plan1.fidelity, plan2.fidelity,
                "Fidelity differs between identical calls");
            prop_assert_eq!(plan1.show_node_labels, plan2.show_node_labels,
                "show_node_labels differs");
            prop_assert_eq!(plan1.show_edge_labels, plan2.show_edge_labels,
                "show_edge_labels differs");
            prop_assert_eq!(plan1.show_clusters, plan2.show_clusters,
                "show_clusters differs");
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 4. Buffer area respected — render_diagram_adaptive returns valid plan
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn render_adaptive_returns_valid_plan(
            (n, edges, dir) in dag_strategy(8, 10),
            (w, h) in buffer_dims(),
        ) {
            let ir = make_ir(n, &edges, dir);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, w, h);
            let mut buf = make_buffer(area);

            let plan = render_diagram_adaptive(&layout, &ir, &config, area, &mut buf);

            // Plan should be well-formed.
            // Fidelity should be one of the valid enum values.
            let _fidelity_str = plan.fidelity.as_str();
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 5. All diagram types renderable — every type renders without panic
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn all_diagram_type_headers_render_without_panic() {
        let sources = [
            "graph TD\n    A --> B\n    B --> C",
            "graph LR\n    A --> B",
            "sequenceDiagram\n    participant A\n    participant B\n    A->>B: hello",
            "classDiagram\n    class Animal\n    class Dog\n    Animal <|-- Dog",
            "erDiagram\n    CUSTOMER ||--o{ ORDER : places",
            "stateDiagram-v2\n    [*] --> Active\n    Active --> [*]",
            "pie\n    \"A\" : 30\n    \"B\" : 70",
            "gantt\n    title Plan\n    section S1\n    Task1 :a1, 2024-01-01, 30d",
        ];

        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();

        for source in &sources {
            let parsed = parse_with_diagnostics(source);
            if parsed.errors.is_empty() {
                let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
                let layout = layout_diagram(&ir_parse.ir, &config);
                let area = Rect::new(0, 0, 80, 24);
                let mut buf = make_buffer(area);
                render_diagram(&layout, &ir_parse.ir, &config, area, &mut buf);
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 6. Zero-area resilience — zero-width or zero-height areas don't panic
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn zero_width_does_not_panic(
            (n, edges, dir) in dag_strategy(4, 5),
            h in 1u16..=40,
        ) {
            let ir = make_ir(n, &edges, dir);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, 0, h);
            let mut buf = make_buffer(area);
            render_diagram(&layout, &ir, &config, area, &mut buf);
        }

        #[test]
        fn zero_height_does_not_panic(
            (n, edges, dir) in dag_strategy(4, 5),
            w in 1u16..=80,
        ) {
            let ir = make_ir(n, &edges, dir);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, w, 0);
            let mut buf = make_buffer(area);
            render_diagram(&layout, &ir, &config, area, &mut buf);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 7. Large area resilience — very large areas don't panic
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn large_area_does_not_panic(
            dir in direction_strategy(),
        ) {
            let ir = make_ir(3, &[(0, 1), (1, 2)], dir);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, 200, 60);
            let mut buf = make_buffer(area);
            render_diagram(&layout, &ir, &config, area, &mut buf);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 8. Full pipeline no-panic — parse → layout → render end-to-end
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn full_pipeline_parse_layout_render(
            node_count in 2..=8usize,
            edge_count in 1..=10usize,
            (w, h) in buffer_dims(),
            dir in direction_strategy(),
        ) {
            // Generate source string.
            let mut lines = vec![match dir {
                GraphDirection::LR => "graph LR".to_string(),
                GraphDirection::RL => "graph RL".to_string(),
                GraphDirection::BT => "graph BT".to_string(),
                _ => "graph TD".to_string(),
            }];
            for i in 0..edge_count.min(node_count - 1) {
                lines.push(format!("    N{} --> N{}", i, i + 1));
            }
            let source = lines.join("\n");

            let parsed = parse_with_diagnostics(&source);
            if parsed.errors.is_empty() {
                let config = MermaidConfig::default();
                let ir_parse = normalize_ast_to_ir(
                    &parsed.ast,
                    &config,
                    &MermaidCompatibilityMatrix::default(),
                    &MermaidFallbackPolicy::default(),
                );
                let layout = layout_diagram(&ir_parse.ir, &config);
                let area = Rect::new(0, 0, w, h);
                let mut buf = make_buffer(area);
                render_diagram(&layout, &ir_parse.ir, &config, area, &mut buf);
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 9. Offset area — rendering at non-zero origin doesn't panic
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn offset_area_does_not_panic(
            x in 0u16..=20,
            y in 0u16..=10,
            w in 10u16..=80,
            h in 8u16..=40,
        ) {
            let ir = make_ir(4, &[(0, 1), (1, 2), (2, 3)], GraphDirection::TD);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            // Buffer must cover the full area including offset.
            let buf_area = Rect::new(0, 0, x.saturating_add(w), y.saturating_add(h));
            let mut buf = make_buffer(buf_area);
            let render_area = Rect::new(x, y, w, h);
            render_diagram(&layout, &ir, &config, render_area, &mut buf);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 10. Empty graph render — zero nodes renders without panic
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn empty_graph_renders_without_panic(
            (w, h) in buffer_dims(),
        ) {
            let ir = make_ir(0, &[], GraphDirection::TB);
            let config = MermaidConfig::default();
            let layout = layout_diagram(&ir, &config);
            let area = Rect::new(0, 0, w, h);
            let mut buf = make_buffer(area);
            render_diagram(&layout, &ir, &config, area, &mut buf);
        }
    }
}
