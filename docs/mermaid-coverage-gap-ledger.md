# Mermaid Coverage Gap Ledger

**Bead**: bd-hudcn.1.2
**Author**: GoldBear
**Date**: 2026-02-06 (updated with bd-hudcn.1.3 refresh)
**Source**: Exhaustive audit of mermaid.rs, mermaid_layout.rs, mermaid_render.rs, mermaid_fixtures.rs

---

## Layer Definitions

| Layer | Description |
|-------|-------------|
| **Header** | `parse_header()` detects the diagram keyword and returns `DiagramType` |
| **Parser** | `parse_with_diagnostics()` dispatches lines to type-specific parse functions, producing `Statement` AST nodes |
| **IR** | `normalize_ast_to_ir()` converts AST statements into `MermaidDiagramIr` (nodes, edges, clusters, labels, pie entries) |
| **Layout** | `layout_diagram()` / `layout_diagram_with_spacing()` assigns coordinates to nodes/edges |
| **Render** | `MermaidRenderer::render()` / `render_with_plan()` draws to terminal `Buffer` |
| **Matrix** | `MermaidCompatibilityMatrix::default()` support level |
| **Fixture** | Test fixture `.mmd` file exists in `tests/fixtures/mermaid/` |

---

## Coverage Matrix

### Tier 1: Supported (matrix = Supported)

| Family | Header | Parser | IR | Layout | Render | Matrix | Fixture | Gaps |
|--------|--------|--------|----|--------|--------|--------|---------|------|
| **Graph** | Y | Full | Full | Full (Sugiyama) | Full | Supported | 5 fixtures | None |
| **ER** | Y | Full | Full | Graph path | Full (cardinality glyphs) | Supported | 1 fixture | None |

### Tier 2a: Partial -- Original Types (matrix = Partial)

| Family | Header | Parser | IR | Layout | Render | Matrix | Fixture | Key Gaps |
|--------|--------|--------|----|--------|--------|--------|---------|----------|
| **Sequence** | Y | Partial | Partial | Partial (lifelines) | Partial | Partial | 1 | Missing participant decl, combined fragments, activation boxes |
| **State** | Y | Partial | Partial | Graph path | Graph path | Partial | 2 | Missing fork/join, choice, state-specific shapes |
| **Gantt** | Y | Full | **None** | **None** | **None** | Partial | 1 | Parser produces AST but IR ignores GanttTask/Title/Section |
| **Class** | Y | Full | Partial | Graph path | Graph path | Partial | 1 | Missing interface/abstract annotations, UML class boxes |
| **Mindmap** | Y | Full | Full | Partial (radial) | Graph path | Partial | 1 | No mindmap-specific node shapes |
| **Pie** | Y | Full | Full | N/A | Full (render_pie) | Partial | 1 | Near-complete; candidate for Supported upgrade |

### Tier 2b: Parser+IR Done, Dedicated Layout Added (matrix = Partial)

| Family | Header | Parser | IR | Layout | Render | Fixture | Remaining Gaps |
|--------|--------|--------|----|--------|--------|---------|----------------|
| **GitGraph** | Y | Full | Full | **Dedicated** (lane-based) | Dedicated (branch lanes) | None | No fixtures; layout could be refined |
| **Journey** | Y | Full | Full | Generic graph | Generic graph | None | No score visualization, no actor lanes |
| **Requirement** | Y | Full | Full | Generic graph | Generic graph | None | No type badges, no risk indicators |

### Tier 3: Unsupported -- Header + Raw Only (matrix = Unsupported)

12 types: Timeline, QuadrantChart, Sankey, XyChart, BlockBeta, PacketBeta, ArchitectureBeta, C4Context, C4Container, C4Component, C4Dynamic, C4Deployment

All detected by parse_header() but body lines become Statement::Raw.

---

## Root Cause Classification

| Code | Description | Affected |
|------|-------------|----------|
| RC-PARSER | No parse dispatch; all -> Raw | 12 Tier 3 types |
| RC-IR | AST parsed but normalize ignores | Gantt |
| RC-IR-PARTIAL | IR exists but missing metadata | Class, Sequence, State |
| RC-LAYOUT-SPECIFIC | Parser+IR done, generic layout | Journey, Requirement |
| RC-RENDER-SPECIFIC | Parser+IR done, generic render | Journey, Requirement |
| RC-FIXTURE | No test fixture | GitGraph, Journey, Requirement, 12 Tier 3 |

---

## Ranked Implementation Order

1. **Gantt** IR fix (parser done, just need IR normalization)
2. **Pie** matrix upgrade (near-complete)
3. **Sequence** enhancements (participant decl, activation boxes)
4. **Class** UML box rendering
5. **State** shape improvements
6. **Mindmap** node shapes
7. **GitGraph** layout refinement (dedicated layout done)
8. **Journey** layout/render (score bars, actor lanes)
9. **Timeline** (new type, moderate complexity)
10. **C4 Family** (5 types, shared infrastructure)
11-17. XyChart, ArchitectureBeta, QuadrantChart, Requirement render, BlockBeta, PacketBeta, Sankey
