#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy)]
pub struct MermaidFixture {
    pub id: &'static str,
    pub file: &'static str,
    pub source: &'static str,
}

pub fn mermaid_fixtures() -> &'static [MermaidFixture] {
    FIXTURES
}

const FIXTURES: &[MermaidFixture] = &[
    MermaidFixture {
        id: "graph_small",
        file: "graph_small.mmd",
        source: include_str!("fixtures/mermaid/graph_small.mmd"),
    },
    MermaidFixture {
        id: "graph_medium",
        file: "graph_medium.mmd",
        source: include_str!("fixtures/mermaid/graph_medium.mmd"),
    },
    MermaidFixture {
        id: "graph_large",
        file: "graph_large.mmd",
        source: include_str!("fixtures/mermaid/graph_large.mmd"),
    },
    MermaidFixture {
        id: "graph_unicode_labels",
        file: "graph_unicode_labels.mmd",
        source: include_str!("fixtures/mermaid/graph_unicode_labels.mmd"),
    },
    MermaidFixture {
        id: "graph_init_directive",
        file: "graph_init_directive.mmd",
        source: include_str!("fixtures/mermaid/graph_init_directive.mmd"),
    },
    MermaidFixture {
        id: "sequence_basic",
        file: "sequence_basic.mmd",
        source: include_str!("fixtures/mermaid/sequence_basic.mmd"),
    },
    MermaidFixture {
        id: "state_basic",
        file: "state_basic.mmd",
        source: include_str!("fixtures/mermaid/state_basic.mmd"),
    },
    MermaidFixture {
        id: "gantt_basic",
        file: "gantt_basic.mmd",
        source: include_str!("fixtures/mermaid/gantt_basic.mmd"),
    },
    MermaidFixture {
        id: "class_basic",
        file: "class_basic.mmd",
        source: include_str!("fixtures/mermaid/class_basic.mmd"),
    },
    MermaidFixture {
        id: "er_basic",
        file: "er_basic.mmd",
        source: include_str!("fixtures/mermaid/er_basic.mmd"),
    },
    MermaidFixture {
        id: "mindmap_basic",
        file: "mindmap_basic.mmd",
        source: include_str!("fixtures/mermaid/mindmap_basic.mmd"),
    },
    MermaidFixture {
        id: "pie_basic",
        file: "pie_basic.mmd",
        source: include_str!("fixtures/mermaid/pie_basic.mmd"),
    },
    MermaidFixture {
        id: "unsupported_mix",
        file: "unsupported_mix.mmd",
        source: include_str!("fixtures/mermaid/unsupported_mix.mmd"),
    },
];

#[cfg(all(test, feature = "diagram"))]
mod tests {
    use super::*;
    use ftui_extras::mermaid::{
        DiagramType, Statement, TokenKind, parse_with_diagnostics, tokenize,
    };
    use std::collections::HashSet;

    #[derive(Default, Debug)]
    struct AstCounts {
        nodes: usize,
        edges: usize,
        directives: usize,
        comments: usize,
        subgraph_start: usize,
        subgraph_end: usize,
        direction: usize,
        class_decl: usize,
        class_def: usize,
        class_assign: usize,
        class_member: usize,
        styles: usize,
        link_styles: usize,
        links: usize,
        sequence: usize,
        gantt_title: usize,
        gantt_section: usize,
        gantt_task: usize,
        pie: usize,
        mindmap: usize,
        raw: usize,
    }

    fn count_statements(statements: &[Statement]) -> AstCounts {
        let mut counts = AstCounts::default();
        for stmt in statements {
            match stmt {
                Statement::Directive(_) => counts.directives += 1,
                Statement::Comment(_) => counts.comments += 1,
                Statement::SubgraphStart { .. } => counts.subgraph_start += 1,
                Statement::SubgraphEnd { .. } => counts.subgraph_end += 1,
                Statement::Direction { .. } => counts.direction += 1,
                Statement::ClassDeclaration { .. } => counts.class_decl += 1,
                Statement::ClassDef { .. } => counts.class_def += 1,
                Statement::ClassAssign { .. } => counts.class_assign += 1,
                Statement::Style { .. } => counts.styles += 1,
                Statement::LinkStyle { .. } => counts.link_styles += 1,
                Statement::Link { .. } => counts.links += 1,
                Statement::Node(_) => counts.nodes += 1,
                Statement::Edge(_) => counts.edges += 1,
                Statement::SequenceMessage(_) => counts.sequence += 1,
                Statement::ClassMember { .. } => counts.class_member += 1,
                Statement::GanttTitle { .. } => counts.gantt_title += 1,
                Statement::GanttSection { .. } => counts.gantt_section += 1,
                Statement::GanttTask(_) => counts.gantt_task += 1,
                Statement::PieEntry(_) => counts.pie += 1,
                Statement::MindmapNode(_) => counts.mindmap += 1,
                Statement::Raw { .. } => counts.raw += 1,
            }
        }
        counts
    }

    #[test]
    fn mermaid_fixture_ids_are_unique() {
        let mut seen = HashSet::new();
        for fixture in mermaid_fixtures() {
            assert!(
                seen.insert(fixture.id),
                "duplicate fixture id: {}",
                fixture.id
            );
        }
    }

    #[test]
    fn mermaid_fixture_headers_match_ids() {
        for fixture in mermaid_fixtures() {
            let first_line = fixture.source.lines().next().unwrap_or_default().trim();
            let expected = format!("%% fixture_id: {}", fixture.id);
            assert_eq!(
                first_line, expected,
                "fixture {} header mismatch",
                fixture.file
            );
        }
    }

    #[test]
    fn mermaid_fixtures_parse_with_headers() {
        for fixture in mermaid_fixtures() {
            let parsed = parse_with_diagnostics(fixture.source);
            assert_ne!(
                parsed.ast.diagram_type,
                DiagramType::Unknown,
                "fixture {} missing or invalid header",
                fixture.file
            );
        }
    }

    #[test]
    fn mermaid_fixtures_have_expected_shapes() {
        for fixture in mermaid_fixtures() {
            let parsed = parse_with_diagnostics(fixture.source);
            let counts = count_statements(&parsed.ast.statements);
            match fixture.id {
                "graph_small" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
                    assert!(counts.edges >= 3, "graph_small edges < 3");
                    assert!(counts.nodes >= 2, "graph_small nodes < 2");
                }
                "graph_medium" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
                    assert!(counts.subgraph_start >= 1, "graph_medium subgraph missing");
                    assert!(
                        counts.subgraph_end >= 1,
                        "graph_medium subgraph end missing"
                    );
                    assert!(counts.direction >= 1, "graph_medium direction missing");
                    assert!(counts.class_def >= 1, "graph_medium classDef missing");
                    assert!(counts.class_assign >= 1, "graph_medium classAssign missing");
                    assert!(counts.styles >= 1, "graph_medium style missing");
                    assert!(counts.link_styles >= 1, "graph_medium linkStyle missing");
                    assert!(counts.links >= 1, "graph_medium link/click missing");
                }
                "graph_large" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
                    assert!(counts.edges >= 10, "graph_large edges < 10");
                }
                "graph_unicode_labels" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
                    assert!(counts.edges >= 2, "graph_unicode_labels edges < 2");
                }
                "graph_init_directive" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
                    assert!(
                        counts.directives >= 1,
                        "graph_init_directive directive missing"
                    );
                }
                "sequence_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Sequence);
                    assert!(counts.sequence >= 2, "sequence_basic messages < 2");
                }
                "state_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::State);
                    assert!(counts.edges >= 2, "state_basic edges < 2");
                }
                "gantt_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Gantt);
                    assert!(counts.gantt_title >= 1, "gantt_basic title missing");
                    assert!(counts.gantt_section >= 1, "gantt_basic section missing");
                    assert!(counts.gantt_task >= 2, "gantt_basic tasks < 2");
                }
                "class_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Class);
                    assert!(counts.class_decl >= 2, "class_basic class decl < 2");
                    assert!(counts.class_member >= 2, "class_basic class member < 2");
                    assert!(counts.edges >= 1, "class_basic edges < 1");
                }
                "er_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Er);
                    assert!(counts.edges >= 2, "er_basic edges < 2");
                }
                "mindmap_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Mindmap);
                    assert!(counts.mindmap >= 4, "mindmap_basic nodes < 4");
                }
                "pie_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Pie);
                    assert!(counts.pie >= 3, "pie_basic entries < 3");
                }
                "unsupported_mix" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Sequence);
                    assert!(counts.sequence >= 1, "unsupported_mix message missing");
                    assert!(counts.raw >= 1, "unsupported_mix raw fallback missing");
                    assert!(counts.links >= 1, "unsupported_mix link/click missing");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn mermaid_fixtures_tokenize_with_eof() {
        for fixture in mermaid_fixtures() {
            let tokens = tokenize(fixture.source);
            assert!(!tokens.is_empty(), "fixture {} has no tokens", fixture.file);
            assert!(
                matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Eof)),
                "fixture {} missing EOF token",
                fixture.file
            );
        }
    }

    #[test]
    fn mermaid_invalid_directive_reports_error() {
        let input = "graph TD\n%%{init: {\"theme\":\"base\"}\nA-->B\n";
        let parsed = parse_with_diagnostics(input);
        assert!(
            !parsed.errors.is_empty(),
            "unterminated directive should report an error"
        );
    }
}
