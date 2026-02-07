#![forbid(unsafe_code)]

/// Fixture tier describing complexity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureTier {
    /// Minimal happy-path exercise.
    Basic,
    /// Moderate complexity (subgraphs, composite states, etc.).
    Medium,
    /// Scale test with many nodes/edges.
    Large,
    /// Pathological: deep nesting, long labels, high edge density, unicode.
    Stress,
    /// Edge-case: unsupported constructs, directives, mixed content.
    EdgeCase,
}

#[derive(Debug, Clone, Copy)]
pub struct MermaidFixture {
    pub id: &'static str,
    pub file: &'static str,
    pub source: &'static str,
    /// Diagram family name matching `DiagramType::as_str()`.
    pub family: &'static str,
    /// Complexity tier for test selection.
    pub tier: FixtureTier,
    /// True when the parser falls through to `Statement::Raw` for body lines
    /// (i.e. diagram type is detected but no dedicated body parser exists yet).
    pub expects_raw_fallback: bool,
}

pub fn mermaid_fixtures() -> &'static [MermaidFixture] {
    FIXTURES
}

const FIXTURES: &[MermaidFixture] = &[
    // -- Graph --
    MermaidFixture {
        id: "graph_small",
        file: "graph_small.mmd",
        source: include_str!("fixtures/mermaid/graph_small.mmd"),
        family: "graph",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "graph_medium",
        file: "graph_medium.mmd",
        source: include_str!("fixtures/mermaid/graph_medium.mmd"),
        family: "graph",
        tier: FixtureTier::Medium,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "graph_large",
        file: "graph_large.mmd",
        source: include_str!("fixtures/mermaid/graph_large.mmd"),
        family: "graph",
        tier: FixtureTier::Large,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "graph_unicode_labels",
        file: "graph_unicode_labels.mmd",
        source: include_str!("fixtures/mermaid/graph_unicode_labels.mmd"),
        family: "graph",
        tier: FixtureTier::EdgeCase,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "graph_init_directive",
        file: "graph_init_directive.mmd",
        source: include_str!("fixtures/mermaid/graph_init_directive.mmd"),
        family: "graph",
        tier: FixtureTier::EdgeCase,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "graph_stress",
        file: "graph_stress.mmd",
        source: include_str!("fixtures/mermaid/graph_stress.mmd"),
        family: "graph",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Sequence --
    MermaidFixture {
        id: "sequence_basic",
        file: "sequence_basic.mmd",
        source: include_str!("fixtures/mermaid/sequence_basic.mmd"),
        family: "sequence",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "sequence_stress",
        file: "sequence_stress.mmd",
        source: include_str!("fixtures/mermaid/sequence_stress.mmd"),
        family: "sequence",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- State --
    MermaidFixture {
        id: "state_basic",
        file: "state_basic.mmd",
        source: include_str!("fixtures/mermaid/state_basic.mmd"),
        family: "state",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "state_composite",
        file: "state_composite.mmd",
        source: include_str!("fixtures/mermaid/state_composite.mmd"),
        family: "state",
        tier: FixtureTier::Medium,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "state_stress",
        file: "state_stress.mmd",
        source: include_str!("fixtures/mermaid/state_stress.mmd"),
        family: "state",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Gantt --
    MermaidFixture {
        id: "gantt_basic",
        file: "gantt_basic.mmd",
        source: include_str!("fixtures/mermaid/gantt_basic.mmd"),
        family: "gantt",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "gantt_stress",
        file: "gantt_stress.mmd",
        source: include_str!("fixtures/mermaid/gantt_stress.mmd"),
        family: "gantt",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Class --
    MermaidFixture {
        id: "class_basic",
        file: "class_basic.mmd",
        source: include_str!("fixtures/mermaid/class_basic.mmd"),
        family: "class",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "class_stress",
        file: "class_stress.mmd",
        source: include_str!("fixtures/mermaid/class_stress.mmd"),
        family: "class",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- ER --
    MermaidFixture {
        id: "er_basic",
        file: "er_basic.mmd",
        source: include_str!("fixtures/mermaid/er_basic.mmd"),
        family: "er",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "er_stress",
        file: "er_stress.mmd",
        source: include_str!("fixtures/mermaid/er_stress.mmd"),
        family: "er",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Mindmap --
    MermaidFixture {
        id: "mindmap_basic",
        file: "mindmap_basic.mmd",
        source: include_str!("fixtures/mermaid/mindmap_basic.mmd"),
        family: "mindmap",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "mindmap_stress",
        file: "mindmap_stress.mmd",
        source: include_str!("fixtures/mermaid/mindmap_stress.mmd"),
        family: "mindmap",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Pie --
    MermaidFixture {
        id: "pie_basic",
        file: "pie_basic.mmd",
        source: include_str!("fixtures/mermaid/pie_basic.mmd"),
        family: "pie",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "pie_stress",
        file: "pie_stress.mmd",
        source: include_str!("fixtures/mermaid/pie_stress.mmd"),
        family: "pie",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Edge-case / mixed --
    MermaidFixture {
        id: "unsupported_mix",
        file: "unsupported_mix.mmd",
        source: include_str!("fixtures/mermaid/unsupported_mix.mmd"),
        family: "sequence",
        tier: FixtureTier::EdgeCase,
        expects_raw_fallback: false,
    },
    // -- GitGraph --
    MermaidFixture {
        id: "gitgraph_basic",
        file: "gitgraph_basic.mmd",
        source: include_str!("fixtures/mermaid/gitgraph_basic.mmd"),
        family: "gitGraph",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "gitgraph_stress",
        file: "gitgraph_stress.mmd",
        source: include_str!("fixtures/mermaid/gitgraph_stress.mmd"),
        family: "gitGraph",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Journey --
    MermaidFixture {
        id: "journey_basic",
        file: "journey_basic.mmd",
        source: include_str!("fixtures/mermaid/journey_basic.mmd"),
        family: "journey",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "journey_stress",
        file: "journey_stress.mmd",
        source: include_str!("fixtures/mermaid/journey_stress.mmd"),
        family: "journey",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "journey_dense_actors",
        file: "journey_dense_actors.mmd",
        source: include_str!("fixtures/mermaid/journey_dense_actors.mmd"),
        family: "journey",
        tier: FixtureTier::EdgeCase,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "journey_malformed",
        file: "journey_malformed.mmd",
        source: include_str!("fixtures/mermaid/journey_malformed.mmd"),
        family: "journey",
        tier: FixtureTier::EdgeCase,
        expects_raw_fallback: false,
    },
    // -- Requirement --
    MermaidFixture {
        id: "requirement_basic",
        file: "requirement_basic.mmd",
        source: include_str!("fixtures/mermaid/requirement_basic.mmd"),
        family: "requirementDiagram",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "requirement_stress",
        file: "requirement_stress.mmd",
        source: include_str!("fixtures/mermaid/requirement_stress.mmd"),
        family: "requirementDiagram",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "requirement_edge_case",
        file: "requirement_edge_case.mmd",
        source: include_str!("fixtures/mermaid/requirement_edge_case.mmd"),
        family: "requirementDiagram",
        tier: FixtureTier::EdgeCase,
        expects_raw_fallback: false,
    },
    // -- Timeline (raw fallback) --
    MermaidFixture {
        id: "timeline_basic",
        file: "timeline_basic.mmd",
        source: include_str!("fixtures/mermaid/timeline_basic.mmd"),
        family: "timeline",
        tier: FixtureTier::Basic,
        expects_raw_fallback: true,
    },
    MermaidFixture {
        id: "timeline_stress",
        file: "timeline_stress.mmd",
        source: include_str!("fixtures/mermaid/timeline_stress.mmd"),
        family: "timeline",
        tier: FixtureTier::Stress,
        expects_raw_fallback: true,
    },
    // -- QuadrantChart (raw fallback) --
    MermaidFixture {
        id: "quadrant_chart_basic",
        file: "quadrant_chart_basic.mmd",
        source: include_str!("fixtures/mermaid/quadrant_chart_basic.mmd"),
        family: "quadrantChart",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "quadrant_chart_stress",
        file: "quadrant_chart_stress.mmd",
        source: include_str!("fixtures/mermaid/quadrant_chart_stress.mmd"),
        family: "quadrantChart",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- Sankey --
    MermaidFixture {
        id: "sankey_basic",
        file: "sankey_basic.mmd",
        source: include_str!("fixtures/mermaid/sankey_basic.mmd"),
        family: "sankey-beta",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "sankey_stress",
        file: "sankey_stress.mmd",
        source: include_str!("fixtures/mermaid/sankey_stress.mmd"),
        family: "sankey-beta",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- XyChart (raw fallback) --
    MermaidFixture {
        id: "xy_chart_basic",
        file: "xy_chart_basic.mmd",
        source: include_str!("fixtures/mermaid/xy_chart_basic.mmd"),
        family: "xychart-beta",
        tier: FixtureTier::Basic,
        expects_raw_fallback: true,
    },
    MermaidFixture {
        id: "xy_chart_stress",
        file: "xy_chart_stress.mmd",
        source: include_str!("fixtures/mermaid/xy_chart_stress.mmd"),
        family: "xychart-beta",
        tier: FixtureTier::Stress,
        expects_raw_fallback: true,
    },
    // -- BlockBeta --
    MermaidFixture {
        id: "block_beta_basic",
        file: "block_beta_basic.mmd",
        source: include_str!("fixtures/mermaid/block_beta_basic.mmd"),
        family: "block-beta",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "block_beta_stress",
        file: "block_beta_stress.mmd",
        source: include_str!("fixtures/mermaid/block_beta_stress.mmd"),
        family: "block-beta",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- PacketBeta (raw fallback) --
    MermaidFixture {
        id: "packet_beta_basic",
        file: "packet_beta_basic.mmd",
        source: include_str!("fixtures/mermaid/packet_beta_basic.mmd"),
        family: "packet-beta",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "packet_beta_stress",
        file: "packet_beta_stress.mmd",
        source: include_str!("fixtures/mermaid/packet_beta_stress.mmd"),
        family: "packet-beta",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- ArchitectureBeta (raw fallback) --
    MermaidFixture {
        id: "architecture_beta_basic",
        file: "architecture_beta_basic.mmd",
        source: include_str!("fixtures/mermaid/architecture_beta_basic.mmd"),
        family: "architecture-beta",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "architecture_beta_stress",
        file: "architecture_beta_stress.mmd",
        source: include_str!("fixtures/mermaid/architecture_beta_stress.mmd"),
        family: "architecture-beta",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- C4Context (partial) --
    MermaidFixture {
        id: "c4_context_basic",
        file: "c4_context_basic.mmd",
        source: include_str!("fixtures/mermaid/c4_context_basic.mmd"),
        family: "C4Context",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "c4_context_stress",
        file: "c4_context_stress.mmd",
        source: include_str!("fixtures/mermaid/c4_context_stress.mmd"),
        family: "C4Context",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- C4Container (partial) --
    MermaidFixture {
        id: "c4_container_basic",
        file: "c4_container_basic.mmd",
        source: include_str!("fixtures/mermaid/c4_container_basic.mmd"),
        family: "C4Container",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "c4_container_stress",
        file: "c4_container_stress.mmd",
        source: include_str!("fixtures/mermaid/c4_container_stress.mmd"),
        family: "C4Container",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- C4Component (partial) --
    MermaidFixture {
        id: "c4_component_basic",
        file: "c4_component_basic.mmd",
        source: include_str!("fixtures/mermaid/c4_component_basic.mmd"),
        family: "C4Component",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "c4_component_stress",
        file: "c4_component_stress.mmd",
        source: include_str!("fixtures/mermaid/c4_component_stress.mmd"),
        family: "C4Component",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- C4Dynamic (partial) --
    MermaidFixture {
        id: "c4_dynamic_basic",
        file: "c4_dynamic_basic.mmd",
        source: include_str!("fixtures/mermaid/c4_dynamic_basic.mmd"),
        family: "C4Dynamic",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "c4_dynamic_stress",
        file: "c4_dynamic_stress.mmd",
        source: include_str!("fixtures/mermaid/c4_dynamic_stress.mmd"),
        family: "C4Dynamic",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
    },
    // -- C4Deployment (partial) --
    MermaidFixture {
        id: "c4_deployment_basic",
        file: "c4_deployment_basic.mmd",
        source: include_str!("fixtures/mermaid/c4_deployment_basic.mmd"),
        family: "C4Deployment",
        tier: FixtureTier::Basic,
        expects_raw_fallback: false,
    },
    MermaidFixture {
        id: "c4_deployment_stress",
        file: "c4_deployment_stress.mmd",
        source: include_str!("fixtures/mermaid/c4_deployment_stress.mmd"),
        family: "C4Deployment",
        tier: FixtureTier::Stress,
        expects_raw_fallback: false,
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
        architecture_group: usize,
        architecture_service: usize,
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
        gitgraph: usize,
        journey: usize,
        requirement: usize,
        timeline: usize,
        xy_chart: usize,
        sankey: usize,
        quadrant: usize,
        packet: usize,
        block_beta: usize,
        raw: usize,
        c4: usize,
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
                Statement::ArchitectureGroup(_) => counts.architecture_group += 1,
                Statement::ArchitectureService(_) => counts.architecture_service += 1,
                Statement::SequenceMessage(_) => counts.sequence += 1,
                Statement::ClassMember { .. } => counts.class_member += 1,
                Statement::GanttTitle { .. } => counts.gantt_title += 1,
                Statement::GanttSection { .. } => counts.gantt_section += 1,
                Statement::GanttTask(_) => counts.gantt_task += 1,
                Statement::PieEntry(_) => counts.pie += 1,
                Statement::MindmapNode(_) => counts.mindmap += 1,
                Statement::GitGraphCommit(_)
                | Statement::GitGraphBranch(_)
                | Statement::GitGraphCheckout(_)
                | Statement::GitGraphMerge(_)
                | Statement::GitGraphCherryPick(_) => counts.gitgraph += 1,
                Statement::JourneySection { .. } | Statement::JourneyTask(_) => counts.journey += 1,
                Statement::RequirementDef(_)
                | Statement::RequirementRelation(_)
                | Statement::RequirementElement(_) => counts.requirement += 1,
                Statement::TimelineSection { .. } | Statement::TimelineEvent(_) => {
                    counts.timeline += 1
                }
                Statement::XyChartXAxis { .. }
                | Statement::XyChartYAxis { .. }
                | Statement::XyChartSeries(_) => counts.xy_chart += 1,
                Statement::SankeyLink(_) => counts.sankey += 1,
                Statement::C4Element(_)
                | Statement::C4Relation(_)
                | Statement::C4BoundaryStart(_)
                | Statement::C4BoundaryEnd { .. }
                | Statement::C4Title { .. } => counts.c4 += 1,
                Statement::QuadrantTitle { .. }
                | Statement::QuadrantAxis { .. }
                | Statement::QuadrantLabel { .. }
                | Statement::QuadrantPoint(_) => counts.quadrant += 1,
                Statement::PacketField(_) => counts.packet += 1,
                Statement::BlockColumns { .. }
                | Statement::BlockDef(_)
                | Statement::BlockGroupStart { .. }
                | Statement::BlockGroupEnd { .. } => counts.block_beta += 1,
                Statement::SequenceParticipant(_)
                | Statement::SequenceNote(_)
                | Statement::SequenceActivation(_)
                | Statement::SequenceControl(_) => counts.sequence += 1,
                Statement::Raw { .. } => counts.raw += 1,
            }
        }
        counts
    }

    fn total_statements(c: &AstCounts) -> usize {
        c.nodes
            + c.edges
            + c.architecture_group
            + c.architecture_service
            + c.directives
            + c.comments
            + c.subgraph_start
            + c.subgraph_end
            + c.direction
            + c.class_decl
            + c.class_def
            + c.class_assign
            + c.class_member
            + c.styles
            + c.link_styles
            + c.links
            + c.sequence
            + c.gantt_title
            + c.gantt_section
            + c.gantt_task
            + c.pie
            + c.mindmap
            + c.gitgraph
            + c.journey
            + c.requirement
            + c.timeline
            + c.xy_chart
            + c.sankey
            + c.c4
            + c.quadrant
            + c.packet
            + c.block_beta
            + c.raw
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
                // -- Graph family --
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
                "graph_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
                    assert!(counts.edges >= 15, "graph_stress edges < 15");
                    assert!(counts.subgraph_start >= 3, "graph_stress deep nesting < 3");
                    assert!(counts.class_def >= 1, "graph_stress classDef missing");
                }
                // -- Sequence family --
                "sequence_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Sequence);
                    assert!(counts.sequence >= 2, "sequence_basic messages < 2");
                }
                "sequence_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Sequence);
                    assert!(counts.sequence >= 8, "sequence_stress statements < 8");
                }
                // -- State family --
                "state_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::State);
                    assert!(counts.edges >= 2, "state_basic edges < 2");
                }
                "state_composite" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::State);
                    assert!(counts.edges >= 3, "state_composite edges < 3");
                }
                "state_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::State);
                    assert!(counts.edges >= 5, "state_stress edges < 5");
                }
                // -- Gantt family --
                "gantt_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Gantt);
                    assert!(counts.gantt_title >= 1, "gantt_basic title missing");
                    assert!(counts.gantt_section >= 1, "gantt_basic section missing");
                    assert!(counts.gantt_task >= 2, "gantt_basic tasks < 2");
                }
                "gantt_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Gantt);
                    assert!(counts.gantt_title >= 1, "gantt_stress title missing");
                    assert!(counts.gantt_section >= 4, "gantt_stress sections < 4");
                    assert!(counts.gantt_task >= 10, "gantt_stress tasks < 10");
                }
                // -- Class family --
                "class_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Class);
                    assert!(counts.class_decl >= 2, "class_basic class decl < 2");
                    assert!(counts.class_member >= 2, "class_basic class member < 2");
                    assert!(counts.edges >= 1, "class_basic edges < 1");
                }
                "class_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Class);
                    assert!(counts.class_decl >= 5, "class_stress class decl < 5");
                    assert!(counts.edges >= 5, "class_stress edges < 5");
                }
                // -- ER family --
                "er_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Er);
                    assert!(counts.edges >= 2, "er_basic edges < 2");
                }
                "er_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Er);
                    assert!(counts.edges >= 5, "er_stress edges < 5");
                }
                // -- Mindmap family --
                "mindmap_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Mindmap);
                    assert!(counts.mindmap >= 4, "mindmap_basic nodes < 4");
                }
                "mindmap_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Mindmap);
                    assert!(counts.mindmap >= 15, "mindmap_stress nodes < 15");
                }
                // -- Pie family --
                "pie_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Pie);
                    assert!(counts.pie >= 3, "pie_basic entries < 3");
                }
                "pie_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Pie);
                    assert!(counts.pie >= 8, "pie_stress entries < 8");
                }
                // -- Edge-case / mixed --
                "unsupported_mix" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Sequence);
                    assert!(counts.sequence >= 1, "unsupported_mix message missing");
                    assert!(counts.links >= 1, "unsupported_mix link/click missing");
                }
                // -- GitGraph family --
                "gitgraph_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::GitGraph);
                    assert!(counts.gitgraph >= 3, "gitgraph_basic gitgraph stmts < 3");
                }
                "gitgraph_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::GitGraph);
                    assert!(counts.gitgraph >= 10, "gitgraph_stress gitgraph stmts < 10");
                }
                // -- Journey family --
                "journey_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Journey);
                    assert!(counts.journey >= 3, "journey_basic journey stmts < 3");
                }
                "journey_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Journey);
                    assert!(counts.journey >= 10, "journey_stress journey stmts < 10");
                }
                "journey_dense_actors" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Journey);
                    assert!(
                        counts.journey >= 6,
                        "journey_dense_actors journey stmts < 6"
                    );
                }
                "journey_malformed" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Journey);
                    assert!(
                        !parsed.errors.is_empty(),
                        "journey_malformed should produce parse errors"
                    );
                    assert!(counts.raw >= 1, "journey_malformed raw fallback missing");
                }
                // -- Requirement family --
                "requirement_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Requirement);
                    assert!(
                        counts.requirement >= 2,
                        "requirement_basic requirement stmts < 2"
                    );
                }
                "requirement_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Requirement);
                    assert!(
                        counts.requirement >= 5,
                        "requirement_stress requirement stmts < 5"
                    );
                }
                "requirement_edge_case" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::Requirement);
                    assert!(
                        counts.requirement >= 5,
                        "requirement_edge_case requirement stmts < 5"
                    );
                }
                // -- PacketBeta family --
                "packet_beta_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::PacketBeta);
                    assert!(counts.packet >= 6, "packet_beta_basic packet stmts < 6");
                }
                "packet_beta_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::PacketBeta);
                    assert!(counts.packet >= 12, "packet_beta_stress packet stmts < 12");
                }
                // -- QuadrantChart family --
                "quadrant_chart_basic" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::QuadrantChart);
                    assert!(
                        counts.quadrant >= 8,
                        "quadrant_chart_basic quadrant stmts < 8"
                    );
                }
                "quadrant_chart_stress" => {
                    assert_eq!(parsed.ast.diagram_type, DiagramType::QuadrantChart);
                    assert!(
                        counts.quadrant >= 14,
                        "quadrant_chart_stress quadrant stmts < 14"
                    );
                }
                // -- All raw-fallback families --
                _ if fixture.expects_raw_fallback => {
                    assert!(
                        total_statements(&counts) >= 1,
                        "{} has no parsed statements",
                        fixture.id
                    );
                    assert!(
                        counts.raw >= 1,
                        "{} expected raw fallback but raw == 0",
                        fixture.id
                    );
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

    #[test]
    fn mermaid_raw_fallback_fixtures_detect_correct_type() {
        for fixture in mermaid_fixtures() {
            if !fixture.expects_raw_fallback {
                continue;
            }
            let parsed = parse_with_diagnostics(fixture.source);
            assert_ne!(
                parsed.ast.diagram_type,
                DiagramType::Unknown,
                "raw-fallback fixture {} should detect diagram type but got Unknown",
                fixture.id
            );
        }
    }

    #[test]
    fn mermaid_stress_fixtures_have_substantial_content() {
        for fixture in mermaid_fixtures() {
            if fixture.tier != FixtureTier::Stress {
                continue;
            }
            let parsed = parse_with_diagnostics(fixture.source);
            let counts = count_statements(&parsed.ast.statements);
            let total = total_statements(&counts);
            assert!(
                total >= 5,
                "stress fixture {} has only {} statements (expected >= 5)",
                fixture.id,
                total
            );
        }
    }

    #[test]
    fn mermaid_every_family_has_basic_and_stress() {
        let mut families: HashSet<&str> = HashSet::new();
        let mut has_basic: HashSet<&str> = HashSet::new();
        let mut has_stress: HashSet<&str> = HashSet::new();
        for fixture in mermaid_fixtures() {
            families.insert(fixture.family);
            match fixture.tier {
                FixtureTier::Basic => {
                    has_basic.insert(fixture.family);
                }
                FixtureTier::Stress => {
                    has_stress.insert(fixture.family);
                }
                _ => {}
            }
        }
        for family in &families {
            assert!(
                has_basic.contains(family) || *family == "sequence",
                "family {} missing basic fixture",
                family
            );
            assert!(
                has_stress.contains(family) || *family == "sequence",
                "family {} missing stress fixture",
                family
            );
        }
    }

    #[test]
    fn mermaid_fixture_metadata_consistency() {
        for fixture in mermaid_fixtures() {
            assert!(
                !fixture.family.is_empty(),
                "fixture {} has empty family",
                fixture.id
            );
            assert!(
                fixture.file.ends_with(".mmd"),
                "fixture {} file doesn't end with .mmd",
                fixture.id
            );
            assert!(
                fixture.id == fixture.file.trim_end_matches(".mmd"),
                "fixture {} id/file mismatch",
                fixture.id
            );
        }
    }

    // ── CI Completion Gate Tests (bd-hudcn.1.6) ─────────────────────

    use ftui_extras::mermaid::{
        DIAGRAM_FAMILY_REGISTRY, DiagramFamilyEntry, MermaidSupportLevel, PipelineStage,
        StageStatus, registry_entry_for,
    };

    /// CI gate: the registry must cover every canonical DiagramType (except Unknown).
    #[test]
    fn ci_gate_registry_covers_all_diagram_types() {
        let mut missing = Vec::new();
        for &dt in DiagramType::all_families() {
            if registry_entry_for(dt).is_none() {
                missing.push(dt.as_str());
            }
        }
        assert!(
            missing.is_empty(),
            "DiagramType variants missing from DIAGRAM_FAMILY_REGISTRY: {missing:?}"
        );
    }

    /// CI gate: no canonical family may remain at `Unsupported`.
    ///
    /// When this test fails a family has regressed or was never promoted.
    /// Update the registry entry to at least `Partial` once parser + IR land.
    #[test]
    fn ci_gate_no_unsupported_families() {
        let unsupported: Vec<&str> = DIAGRAM_FAMILY_REGISTRY
            .iter()
            .filter(|e| e.support_level == MermaidSupportLevel::Unsupported)
            .map(|e| e.family.as_str())
            .collect();
        assert!(
            unsupported.is_empty(),
            "Unsupported families found — each must reach at least Partial: {unsupported:?}"
        );
    }

    /// CI gate: every `Supported` family must have a fully complete pipeline.
    ///
    /// Each of the 8 pipeline stages must be `Done` or `NotApplicable`.
    /// Failure output names the family and the first incomplete stage.
    #[test]
    fn ci_gate_supported_families_have_complete_pipeline() {
        let mut failures = Vec::new();
        for entry in DIAGRAM_FAMILY_REGISTRY {
            if entry.support_level != MermaidSupportLevel::Supported {
                continue;
            }
            for &stage in PipelineStage::ALL {
                let status = entry.stage_status(stage);
                if !status.is_complete() {
                    failures.push(format!(
                        "{}: stage {} is {} (expected done/n_a)",
                        entry.family.as_str(),
                        stage.as_str(),
                        status.as_str(),
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "Supported families with incomplete pipeline stages:\n  {}",
            failures.join("\n  ")
        );
    }

    /// CI gate: every family in the registry must have at least one fixture file.
    ///
    /// Matches registry families to fixtures via `DiagramType::as_str()`.
    #[test]
    fn ci_gate_every_registry_family_has_fixtures() {
        let fixture_families: HashSet<&str> = mermaid_fixtures().iter().map(|f| f.family).collect();
        let mut missing = Vec::new();
        for entry in DIAGRAM_FAMILY_REGISTRY {
            let family_str = entry.family.as_str();
            // Fixture family strings use DiagramType::as_str() or the canonical
            // keyword (e.g. "sankey-beta"). Check both.
            let has_fixture = fixture_families.contains(family_str)
                || fixture_families.contains(entry.canonical_keyword);
            if !has_fixture {
                missing.push(family_str);
            }
        }
        assert!(
            missing.is_empty(),
            "Registry families with no fixture files: {missing:?}"
        );
    }

    /// CI gate: every `Supported` family whose fixtures stage is `Done` must
    /// have snapshot files at the 80x24 tier (minimum required coverage).
    ///
    /// Uses the snapshot directory listing to verify.
    #[test]
    fn ci_gate_supported_families_have_snapshots() {
        // Map from DiagramType to the snapshot filename prefix used in
        // `assert_buffer_snapshot_text`. These must stay in sync with the
        // snapshot test naming convention.
        fn snapshot_prefix(entry: &DiagramFamilyEntry) -> &'static str {
            match entry.family {
                DiagramType::Graph => "mermaid_flowchart",
                DiagramType::Sequence => "mermaid_sequence",
                DiagramType::State => "mermaid_state",
                DiagramType::Gantt => "mermaid_gantt",
                DiagramType::Class => "mermaid_class",
                DiagramType::Er => "mermaid_er",
                DiagramType::Mindmap => "mermaid_mindmap",
                DiagramType::Pie => "mermaid_pie",
                DiagramType::GitGraph => "mermaid_gitgraph",
                DiagramType::Journey => "mermaid_journey",
                DiagramType::Requirement => "mermaid_requirement",
                DiagramType::Timeline => "mermaid_timeline",
                DiagramType::QuadrantChart => "mermaid_quadrant",
                DiagramType::Sankey => "mermaid_sankey",
                DiagramType::XyChart => "mermaid_xychart",
                DiagramType::BlockBeta => "mermaid_block_beta",
                DiagramType::PacketBeta => "mermaid_packet",
                DiagramType::ArchitectureBeta => "mermaid_arch_beta",
                DiagramType::C4Context => "mermaid_c4_context",
                DiagramType::C4Container => "mermaid_c4_container",
                DiagramType::C4Component => "mermaid_c4_component",
                DiagramType::C4Dynamic => "mermaid_c4_dynamic",
                DiagramType::C4Deployment => "mermaid_c4_deployment",
                DiagramType::Unknown => "mermaid_unknown",
            }
        }

        let snap_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
        let snap_files: Vec<String> = std::fs::read_dir(&snap_dir)
            .expect("snapshot directory missing")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        let mut missing = Vec::new();
        for entry in DIAGRAM_FAMILY_REGISTRY {
            if entry.support_level != MermaidSupportLevel::Supported {
                continue;
            }
            if entry.stage_status(PipelineStage::Snapshots) != StageStatus::Done {
                continue;
            }
            let prefix = snapshot_prefix(entry);
            let has_snap = snap_files.iter().any(|f| f.starts_with(prefix));
            if !has_snap {
                missing.push(format!(
                    "{} (expected prefix: {prefix})",
                    entry.family.as_str(),
                ));
            }
        }
        assert!(
            missing.is_empty(),
            "Supported families with Done snapshots stage but no snapshot files:\n  {}",
            missing.join("\n  ")
        );
    }

    /// CI gate: the pipeline stage labelled `Fixtures` must be `Done` for every
    /// family that actually has fixture files, and vice versa.
    #[test]
    fn ci_gate_fixture_stage_matches_reality() {
        let fixture_families: HashSet<&str> = mermaid_fixtures().iter().map(|f| f.family).collect();
        let mut mismatches = Vec::new();
        for entry in DIAGRAM_FAMILY_REGISTRY {
            let family_str = entry.family.as_str();
            let has_fixture = fixture_families.contains(family_str)
                || fixture_families.contains(entry.canonical_keyword);
            let stage = entry.stage_status(PipelineStage::Fixtures);
            if has_fixture && stage == StageStatus::NotStarted {
                mismatches.push(format!(
                    "{family_str}: has fixture files but pipeline.fixtures == not_started"
                ));
            }
            if !has_fixture && stage == StageStatus::Done {
                mismatches.push(format!(
                    "{family_str}: pipeline.fixtures == done but no fixture files found"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "Pipeline fixtures stage / actual fixture mismatches:\n  {}",
            mismatches.join("\n  ")
        );
    }
}
