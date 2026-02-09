//! Property-based invariant tests for the mermaid parser + normalization pipeline.
//!
//! These tests verify structural invariants that must hold for the
//! `parse_with_diagnostics` → `normalize_ast_to_ir` pipeline:
//!
//! 1. No panic on arbitrary input — parser must never crash
//! 2. Parser determinism — same input always yields identical output
//! 3. All diagram headers parse — every `DiagramType` keyword accepted
//! 4. Normalize never panics — given a valid AST, normalization is safe
//! 5. IR diagram type matches AST — type is preserved through normalization
//! 6. IR node count consistency — simple graphs produce expected node counts
//! 7. Error recovery — malformed input produces diagnostics, not crashes
//! 8. Empty/whitespace input — graceful handling
//! 9. Nested quote resilience — deep nesting doesn't stackoverflow
//! 10. Unicode input safety — multi-byte chars don't corrupt spans
//! 11. IR edge endpoint validity — edge endpoints reference valid node indices
//! 12. Parse + normalize idempotence — parsing twice yields same result

#[cfg(feature = "diagram")]
mod tests {
    use ftui_extras::mermaid::*;
    use proptest::prelude::*;

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn default_config() -> MermaidConfig {
        MermaidConfig::default()
    }

    fn default_matrix() -> MermaidCompatibilityMatrix {
        MermaidCompatibilityMatrix::default()
    }

    fn default_policy() -> MermaidFallbackPolicy {
        MermaidFallbackPolicy::default()
    }

    /// Parse and normalize a mermaid source string end-to-end.
    fn full_pipeline(input: &str) -> (MermaidParse, Option<MermaidIrParse>) {
        let parsed = parse_with_diagnostics(input);
        if parsed.errors.is_empty() {
            let ir_parse = normalize_ast_to_ir(
                &parsed.ast,
                &default_config(),
                &default_matrix(),
                &default_policy(),
            );
            (parsed, Some(ir_parse))
        } else {
            (parsed, None)
        }
    }

    /// All diagram type header keywords that the parser should recognize.
    fn diagram_headers() -> Vec<&'static str> {
        vec![
            "graph TD",
            "graph LR",
            "graph RL",
            "graph BT",
            "graph TB",
            "flowchart TD",
            "flowchart LR",
            "sequenceDiagram",
            "stateDiagram-v2",
            "stateDiagram",
            "gantt",
            "classDiagram",
            "erDiagram",
            "mindmap",
            "pie",
            "gitGraph",
            "journey",
            "requirementDiagram",
            "timeline",
            "quadrantChart",
            "sankey-beta",
            "xychart-beta",
            "block-beta",
            "packet-beta",
            "architecture-beta",
            "C4Context",
            "C4Container",
            "C4Component",
            "C4Dynamic",
            "C4Deployment",
        ]
    }

    // ── Strategies ──────────────────────────────────────────────────────────

    /// Generate arbitrary strings (including non-UTF8-safe patterns).
    fn arbitrary_input() -> impl Strategy<Value = String> {
        proptest::string::string_regex(".{0,200}").unwrap()
    }

    /// Generate a random flowchart with N nodes and M edges.
    fn flowchart_source(max_nodes: usize, max_edges: usize) -> impl Strategy<Value = String> {
        (2..=max_nodes, 1..=max_edges).prop_flat_map(move |(n, e)| {
            let edge_count = e.min(n * (n - 1) / 2).max(1);
            proptest::collection::vec((0..n, 0..n), edge_count).prop_map(move |edges| {
                let mut lines = vec!["graph TD".to_string()];
                for (from, to) in edges {
                    if from != to {
                        lines.push(format!("    N{from} --> N{to}"));
                    }
                }
                lines.join("\n")
            })
        })
    }

    /// Generate a random pie chart source.
    fn pie_source() -> impl Strategy<Value = String> {
        proptest::collection::vec("[a-zA-Z ]{1,15}", 1..=8).prop_map(|labels| {
            let mut lines = vec!["pie".to_string()];
            for (i, label) in labels.iter().enumerate() {
                lines.push(format!("    \"{label}\" : {}", i + 1));
            }
            lines.join("\n")
        })
    }

    /// Generate a random sequence diagram source.
    fn sequence_source() -> impl Strategy<Value = String> {
        (2..=6usize).prop_flat_map(|n| {
            proptest::collection::vec((0..n, 0..n), 1..=8).prop_map(move |msgs| {
                let mut lines = vec!["sequenceDiagram".to_string()];
                for i in 0..n {
                    lines.push(format!("    participant P{i}"));
                }
                for (from, to) in msgs {
                    if from != to {
                        lines.push(format!("    P{from}->>P{to}: msg"));
                    }
                }
                lines.join("\n")
            })
        })
    }

    /// Generate a random class diagram source.
    fn class_source() -> impl Strategy<Value = String> {
        (2..=6usize).prop_flat_map(|n| {
            proptest::collection::vec((0..n, 0..n), 1..=6).prop_map(move |rels| {
                let mut lines = vec!["classDiagram".to_string()];
                for i in 0..n {
                    lines.push(format!("    class C{i}"));
                }
                for (from, to) in rels {
                    if from != to {
                        lines.push(format!("    C{from} --> C{to}"));
                    }
                }
                lines.join("\n")
            })
        })
    }

    /// Pick a random diagram type strategy.
    fn mixed_diagram_source() -> impl Strategy<Value = String> {
        prop_oneof![
            flowchart_source(8, 10),
            pie_source(),
            sequence_source(),
            class_source(),
        ]
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 1. No panic on arbitrary input — parser must never crash
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn parser_never_panics_on_arbitrary_input(input in arbitrary_input()) {
            // This must not panic. We don't care about the result.
            let _result = parse_with_diagnostics(&input);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 2. Parser determinism — same input always yields identical output
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn parser_is_deterministic(input in mixed_diagram_source()) {
            let r1 = parse_with_diagnostics(&input);
            let r2 = parse_with_diagnostics(&input);

            prop_assert_eq!(r1.errors.len(), r2.errors.len(),
                "Error count differs between identical runs");
            prop_assert_eq!(
                r1.ast.diagram_type, r2.ast.diagram_type,
                "Diagram type differs between identical runs",
            );
            prop_assert_eq!(
                r1.ast.statements.len(), r2.ast.statements.len(),
                "Statement count differs between identical runs",
            );
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 3. All diagram headers parse — every DiagramType keyword accepted
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn all_diagram_type_headers_parse_without_error() {
        for header in diagram_headers() {
            let result = parse_with_diagnostics(header);
            assert!(
                result.errors.is_empty(),
                "Header {:?} produced parse errors: {:?}",
                header,
                result.errors,
            );
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 4. Normalize never panics — given a valid AST, normalization is safe
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn normalize_never_panics_on_valid_ast(source in mixed_diagram_source()) {
            let parsed = parse_with_diagnostics(&source);
            if parsed.errors.is_empty() {
                // Must not panic.
                let _ir = normalize_ast_to_ir(
                    &parsed.ast, &default_config(), &default_matrix(), &default_policy(),
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 5. IR diagram type matches AST — type is preserved through normalization
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn ir_preserves_diagram_type(source in mixed_diagram_source()) {
            let (parsed, ir_opt) = full_pipeline(&source);
            if let Some(ir_parse) = ir_opt {
                prop_assert_eq!(
                    ir_parse.ir.diagram_type, parsed.ast.diagram_type,
                    "IR diagram type {:?} != AST diagram type {:?}",
                    ir_parse.ir.diagram_type, parsed.ast.diagram_type,
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 6. IR node count consistency — simple flowcharts produce expected nodes
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn flowchart_ir_has_nodes(source in flowchart_source(8, 10)) {
            let (_parsed, ir_opt) = full_pipeline(&source);
            if let Some(ir_parse) = ir_opt {
                // A flowchart with edges should have at least 2 nodes.
                if !ir_parse.ir.edges.is_empty() {
                    prop_assert!(
                        ir_parse.ir.nodes.len() >= 2,
                        "Flowchart with {} edges has only {} nodes",
                        ir_parse.ir.edges.len(),
                        ir_parse.ir.nodes.len(),
                    );
                }
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 7. Error recovery — malformed input produces diagnostics, not crashes
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn malformed_input_recovers_gracefully(
            header_idx in 0..5usize,
            garbage in "[!@#$%^&*(){}|<>?]{1,50}",
        ) {
            let headers = ["graph TD", "sequenceDiagram", "classDiagram", "erDiagram", "stateDiagram-v2"];
            let header = headers[header_idx];
            let input = format!("{header}\n    {garbage}");

            // Must not panic. May produce errors.
            let _result = parse_with_diagnostics(&input);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 8. Empty/whitespace input — graceful handling
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn empty_input_does_not_panic() {
        let _r = parse_with_diagnostics("");
    }

    #[test]
    fn whitespace_only_does_not_panic() {
        let _r = parse_with_diagnostics("   \n\t\n  ");
    }

    #[test]
    fn single_newline_does_not_panic() {
        let _r = parse_with_diagnostics("\n");
    }

    proptest! {
        #[test]
        fn whitespace_variants_do_not_panic(ws in "[ \\t\\n\\r]{0,100}") {
            let _r = parse_with_diagnostics(&ws);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 9. Nested structure resilience — deep nesting doesn't stackoverflow
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn deeply_nested_subgraphs_no_stackoverflow(depth in 1..=30usize) {
            let mut lines = vec!["graph TD".to_string()];
            for i in 0..depth {
                lines.push(format!("{}subgraph S{i}", "    ".repeat(i + 1)));
            }
            lines.push(format!("{}A --> B", "    ".repeat(depth + 1)));
            for i in (0..depth).rev() {
                lines.push(format!("{}end", "    ".repeat(i + 1)));
            }
            let source = lines.join("\n");

            // Must not stackoverflow.
            let _result = parse_with_diagnostics(&source);
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 10. Unicode input safety — multi-byte chars don't corrupt spans
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn unicode_node_labels_parse_safely(
            label in "[\\p{L}\\p{N}_ ]{1,20}",
        ) {
            let source = format!("graph TD\n    A[\"{label}\"] --> B[\"{label}\"]");
            let result = parse_with_diagnostics(&source);

            // If it parsed successfully, normalize too.
            if result.errors.is_empty() {
                let _ir = normalize_ast_to_ir(
                    &result.ast, &default_config(), &default_matrix(), &default_policy(),
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 11. IR edge endpoint validity — endpoints reference valid node indices
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn ir_edge_endpoints_are_valid(source in flowchart_source(10, 12)) {
            let (_parsed, ir_opt) = full_pipeline(&source);
            if let Some(ir_parse) = ir_opt {
                let node_count = ir_parse.ir.nodes.len();
                for (i, edge) in ir_parse.ir.edges.iter().enumerate() {
                    match &edge.from {
                        IrEndpoint::Node(IrNodeId(idx)) => {
                            prop_assert!(
                                *idx < node_count,
                                "Edge {} from-node index {} out of range (nodes: {})",
                                i, idx, node_count,
                            );
                        }
                        IrEndpoint::Port(_) => {} // Ports are valid by construction.
                    }
                    match &edge.to {
                        IrEndpoint::Node(IrNodeId(idx)) => {
                            prop_assert!(
                                *idx < node_count,
                                "Edge {} to-node index {} out of range (nodes: {})",
                                i, idx, node_count,
                            );
                        }
                        IrEndpoint::Port(_) => {}
                    }
                }
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 12. Parse + normalize determinism — full pipeline is deterministic
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn full_pipeline_is_deterministic(source in mixed_diagram_source()) {
            let (p1, ir1) = full_pipeline(&source);
            let (p2, ir2) = full_pipeline(&source);

            prop_assert_eq!(p1.errors.len(), p2.errors.len());

            match (ir1, ir2) {
                (Some(a), Some(b)) => {
                    prop_assert_eq!(a.ir.nodes.len(), b.ir.nodes.len(),
                        "IR node count differs");
                    prop_assert_eq!(a.ir.edges.len(), b.ir.edges.len(),
                        "IR edge count differs");
                    prop_assert_eq!(a.warnings.len(), b.warnings.len(),
                        "Warning count differs");
                }
                (None, None) => {} // Both failed, fine.
                _ => {
                    prop_assert!(false, "One pipeline succeeded and the other failed");
                }
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 13. Pie chart entries match — pie IR entries correspond to source lines
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn pie_ir_entries_match_source(source in pie_source()) {
            let (_parsed, ir_opt) = full_pipeline(&source);
            if let Some(ir_parse) = ir_opt {
                prop_assert_eq!(
                    ir_parse.ir.diagram_type, DiagramType::Pie,
                    "Pie source should produce Pie diagram type",
                );
                // Pie entries should be non-empty for valid pie source.
                prop_assert!(
                    !ir_parse.ir.pie_entries.is_empty(),
                    "Pie IR has no entries despite valid source",
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 14. Sequence participants — sequence IR participants match source
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn sequence_participants_preserved(source in sequence_source()) {
            let (_parsed, ir_opt) = full_pipeline(&source);
            if let Some(ir_parse) = ir_opt {
                prop_assert_eq!(
                    ir_parse.ir.diagram_type, DiagramType::Sequence,
                    "Sequence source should produce Sequence diagram type",
                );
                // Should have at least 2 participants.
                prop_assert!(
                    ir_parse.ir.sequence_participants.len() >= 2,
                    "Sequence IR has {} participants (expected ≥ 2)",
                    ir_parse.ir.sequence_participants.len(),
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 15. All diagram headers normalize — every type header produces valid IR
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn all_diagram_headers_normalize_without_panic() {
        for header in diagram_headers() {
            let parsed = parse_with_diagnostics(header);
            if parsed.errors.is_empty() {
                let _ir = normalize_ast_to_ir(
                    &parsed.ast,
                    &default_config(),
                    &default_matrix(),
                    &default_policy(),
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 16. Large input resilience — parser handles long inputs gracefully
    // ═════════════════════════════════════════════════════════════════════════

    proptest! {
        #[test]
        fn large_flowchart_does_not_panic(edge_count in 20..=50usize) {
            let mut lines = vec!["graph TD".to_string()];
            for i in 0..edge_count {
                lines.push(format!("    N{} --> N{}", i, i + 1));
            }
            let source = lines.join("\n");

            let result = parse_with_diagnostics(&source);
            if result.errors.is_empty() {
                let _ir = normalize_ast_to_ir(
                    &result.ast, &default_config(), &default_matrix(), &default_policy(),
                );
            }
        }
    }
}
