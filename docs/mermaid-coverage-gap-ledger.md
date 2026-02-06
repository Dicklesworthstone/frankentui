# Mermaid Coverage Gap Ledger

**Bead**: bd-hudcn.1.2
**Author**: GoldBear
**Date**: 2026-02-06
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

| Family | Bead | Header | Parser | IR | Layout | Render | Matrix | Fixture | Gaps |
|--------|------|--------|--------|----|--------|--------|--------|---------|------|
| **Graph** | - | Y | Full (edges, nodes, subgraphs, classDef, classAssign, style, linkStyle, direction) | Full (nodes, edges, clusters, ports, styles) | Full (Sugiyama layered, all 5 directions) | Full (nodes, edges, clusters, labels, selection) | Supported | 5 fixtures | None |
| **ER** | - | Y | Full (edges, entity blocks, attributes) | Full (nodes, edges, class members) | Uses Graph layout path | Full (ER-specific cardinality glyphs, relationship labels) | Supported | 1 fixture | None |

### Tier 2: Partial (matrix = Partial)

| Family | Bead | Header | Parser | IR | Layout | Render | Matrix | Fixture | Gaps |
|--------|------|--------|--------|----|--------|--------|--------|---------|------|
| **Sequence** | bd-hudcn.1.6 | Y | Partial (`parse_sequence` → `SequenceMessage`; no participant decl, no alt/loop/opt blocks, no activate/deactivate, no notes) | Partial (messages → nodes + edges; no lifeline ordering metadata) | Partial (special lifeline path in layout) | Partial (lifeline rendering, but no activation boxes, no combined fragments) | Partial | 1 fixture | **Parser**: missing participant declarations, alt/loop/opt/par combined fragments, activate/deactivate, notes. **IR**: no fragment representation. **Render**: no activation boxes, no combined fragment frames. |
| **State** | bd-hudcn.1.5 | Y | Partial (state declarations, edges, composite states via `{`, notes; no fork/join, no choice, no concurrent `--`) | Partial (states → nodes, transitions → edges, composites → clusters) | Uses Graph layout path | Uses Graph render path (no state-specific shapes) | Partial | 2 fixtures | **Parser**: missing fork/join pseudo-states, choice nodes, concurrent separator. **Render**: no rounded-rect state shapes, no start/end bullets. |
| **Gantt** | bd-hudcn.1.6 | Y | Full (`parse_gantt` → GanttTitle, GanttSection, GanttTask with status/dates) | **None** (GanttTask/GanttTitle/GanttSection fall through to `_ => {}` catch-all) | **None** (no Gantt-specific layout; empty IR → no layout) | **None** (no Gantt-specific render path) | Partial | 1 fixture | **IR**: GanttTask/GanttTitle/GanttSection not normalized into IR nodes/edges. **Layout**: no temporal lane layout. **Render**: no timeline/bar chart rendering. Root cause: parser produces AST but IR normalization ignores it. |
| **Class** | bd-hudcn.1.5 | Y | Full (ClassDeclaration, ClassMember, edges, inheritance arrows) | Partial (classes → nodes with members, inheritance → edges; no interface/abstract annotations) | Uses Graph layout path | Uses Graph render path (no UML class box rendering) | Partial | 1 fixture | **IR**: no interface/abstract/stereotype metadata. **Render**: no UML class box with compartments (name/attributes/methods). |
| **Mindmap** | bd-hudcn.1.6 | Y | Full (`parse_mindmap` → MindmapNode with depth/text) | Full (nodes with parent-child edges based on indentation) | Partial (special mindmap path in layout, but uses radial heuristic) | Uses Graph render path (no mindmap-specific shapes like clouds/banners) | Partial | 1 fixture | **Render**: no mindmap-specific node shapes (cloud, banner, hexagon, circle). Layout could use a dedicated radial tree algorithm. |
| **Pie** | bd-hudcn.1.6 | Y | Full (`parse_pie` → PieEntry; showData line; title line) | Full (pie entries with labels/values; title interned) | **N/A** (Pie uses direct render, no layout) | Full (dedicated `render_pie` with proportional bars, labels, legend) | Partial | 1 fixture | Minor: pie_title interning was buggy (fixed in deep review). Matrix says Partial but functional coverage is near-complete. |

### Tier 3: Unsupported — Header + Raw Only (matrix = Unsupported)

These types are detected by `parse_header()` but ALL body lines become `Statement::Raw`. No type-specific parsing, no IR normalization, no layout, no render.

| Family | Bead | Header | Parser | IR | Layout | Render | Fixture | Root Cause | Complexity | User Impact |
|--------|------|--------|--------|----|--------|--------|---------|------------|------------|-------------|
| **GitGraph** | bd-hudcn.1.7 | Y | Raw only | None | None | None | None | Missing parser dispatch + Statement variants + IR normalization | Medium (commit/branch/merge model maps to nodes+edges) | Medium (common in dev docs) |
| **Journey** | bd-hudcn.1.8 | Y | Raw only | None | None | None | None | Missing parser dispatch + Statement variants + IR normalization | Low-Medium (section/task/actor maps to lanes) | Medium (common in UX docs) |
| **Requirement** | bd-hudcn.1.9 | Y | Raw only | None | None | None | None | Missing parser dispatch + Statement variants + IR normalization | Medium (requirement/element/relationship model) | Low (niche usage) |
| **Timeline** | bd-hudcn.1.10 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (chronological events/sections) | Medium (roadmap docs) |
| **QuadrantChart** | bd-hudcn.1.11 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (4-quadrant axes + labeled points) | Low-Medium (prioritization) |
| **Sankey** | bd-hudcn.1.13 | Y | Raw only | None | None | None | None | Missing everything below header | High (weighted flow paths, proportional rendering) | Low (specialized) |
| **XyChart** | bd-hudcn.1.12 | Y | Raw only | None | None | None | None | Missing everything below header | High (axis/tick rendering, line/bar series) | Medium (metrics docs) |
| **BlockBeta** | bd-hudcn.1.15 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (nested block containers) | Low (beta feature) |
| **PacketBeta** | bd-hudcn.1.16 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (header fields layout) | Low (network docs) |
| **ArchitectureBeta** | bd-hudcn.1.17 | Y | Raw only | None | None | None | None | Missing everything below header | Medium-High (service groups, edges, icons) | Medium (architecture docs) |
| **C4Context** | bd-hudcn.1.14 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (persons/systems/boundaries) | Medium (architecture) |
| **C4Container** | bd-hudcn.1.14 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (containers within systems) | Medium (architecture) |
| **C4Component** | bd-hudcn.1.14 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (components within containers) | Low-Medium |
| **C4Dynamic** | bd-hudcn.1.14 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (numbered interaction flows) | Low |
| **C4Deployment** | bd-hudcn.1.14 | Y | Raw only | None | None | None | None | Missing everything below header | Medium (deployment nodes, infra) | Low |

---

## Root Cause Classification

| Code | Description | Affected Families |
|------|-------------|-------------------|
| **RC-PARSER** | No type-specific parse dispatch; all lines → `Statement::Raw` | GitGraph, Journey, Requirement, Timeline, QuadrantChart, Sankey, XyChart, BlockBeta, PacketBeta, ArchitectureBeta, C4* |
| **RC-AST** | Missing `Statement` enum variants for the type's constructs | GitGraph, Journey, Requirement, Timeline, QuadrantChart, Sankey, XyChart, BlockBeta, PacketBeta, ArchitectureBeta, C4* |
| **RC-IR** | AST parsed but `normalize_ast_to_ir()` ignores the statements (catch-all `_ => {}`) | Gantt (parsed but not normalized) |
| **RC-IR-PARTIAL** | IR normalization exists but missing metadata | Class (no interface/abstract), Sequence (no fragments), State (no fork/join) |
| **RC-LAYOUT** | No type-specific layout algorithm | Gantt, all Tier 3 types |
| **RC-RENDER** | No type-specific render path | Gantt, State (no state shapes), Class (no UML boxes), Mindmap (no special shapes), all Tier 3 types |
| **RC-FIXTURE** | No test fixture for the diagram type | All 15 Unsupported types |
| **RC-MATRIX** | Compatibility matrix marks as Unsupported, triggering fallback warnings | All 15 Unsupported types |

---

## Ranked Implementation Order

Ranking criteria:
- **User Impact** (1-5): How commonly this diagram type appears in real-world Markdown docs
- **Complexity** (1-5): Implementation effort across parser/IR/layout/render (1=easy, 5=hard)
- **Unblock Value**: Number of downstream beads this unblocks
- **Score** = (User Impact * 2 + Unblock Value) / Complexity

### Priority 1: Fix Partial Types (highest ROI, smallest gaps)

| Rank | Family | Score | Rationale |
|------|--------|-------|-----------|
| 1 | **Gantt** (IR fix) | 8.0 | Parser already complete; just need IR normalization + temporal layout + bar render. Unblocks Partial→Supported upgrade. |
| 2 | **Pie** (matrix upgrade) | 7.0 | Near-complete; just upgrade matrix from Partial to Supported after title-interning fix. |
| 3 | **Sequence** (enhance) | 5.0 | Most-used after Graph; adding participant decl + activation boxes has high impact. |
| 4 | **Class** (UML boxes) | 4.5 | Common in codebase docs; needs UML class box renderer with compartments. |
| 5 | **State** (shapes) | 4.0 | Common; needs rounded-rect shapes + start/end pseudo-state bullets. |
| 6 | **Mindmap** (shapes) | 3.5 | Layout works; just needs mindmap-specific node shapes. |

### Priority 2: New Types — High Impact

| Rank | Family | Score | Rationale |
|------|--------|-------|-----------|
| 7 | **GitGraph** | 3.2 | Common in dev docs. Commit/branch model maps well to DAG nodes+edges. |
| 8 | **Journey** | 3.0 | Common in UX/product docs. Simple section/task/rating structure. |
| 9 | **Timeline** | 2.8 | Used in roadmap docs. Chronological lane layout is moderate complexity. |
| 10 | **C4 Family** (5 types) | 2.5 | High architecture-doc value. All 5 C4 types share parser infrastructure. |
| 11 | **XyChart** | 2.2 | Useful for metrics; requires axis/tick/series rendering (high complexity). |
| 12 | **ArchitectureBeta** | 2.0 | Valuable for system diagrams; needs service-group layout. |

### Priority 3: New Types — Niche / Beta

| Rank | Family | Score | Rationale |
|------|--------|-------|-----------|
| 13 | **QuadrantChart** | 1.8 | Prioritization tool; 4-quadrant axis rendering is moderate. |
| 14 | **Requirement** | 1.5 | Niche (requirements engineering). Relationship model is straightforward. |
| 15 | **BlockBeta** | 1.3 | Beta feature; nested block containers need custom layout. |
| 16 | **PacketBeta** | 1.0 | Beta; header-field layout is specialized for network docs. |
| 17 | **Sankey** | 0.8 | Specialized weighted-flow rendering is high complexity for niche use. |

---

## Fixture Coverage Gaps

Missing `.mmd` test fixtures for:
- gitgraph, journey, requirementDiagram, timeline, quadrantChart, sankey, xychart, block-beta, packet-beta, architecture, C4Context, C4Container, C4Component, C4Dynamic, C4Deployment

Existing fixtures that should be expanded:
- `state_basic.mmd` / `state_composite.mmd`: add fork/join, choice, concurrent
- `sequence_basic.mmd`: add participant declarations, alt/loop blocks
- `class_basic.mmd`: add interface/abstract annotations
- `gantt_basic.mmd`: verify round-trip through IR normalization (currently zero IR output)

---

## Bead Mapping

Every gap maps to exactly one implementation bead:

| Gap | Bead ID | Title |
|-----|---------|-------|
| GitGraph full support | bd-hudcn.1.7 | Diagram Type: gitGraph |
| Journey full support | bd-hudcn.1.8 | Diagram Type: journey |
| Requirement full support | bd-hudcn.1.9 | Diagram Type: requirementDiagram |
| Timeline full support | bd-hudcn.1.10 | Diagram Type: timeline |
| QuadrantChart full support | bd-hudcn.1.11 | Diagram Type: quadrantChart |
| XyChart full support | bd-hudcn.1.12 | Diagram Type: xyChart |
| Sankey full support | bd-hudcn.1.13 | Diagram Type: sankey |
| C4 family full support | bd-hudcn.1.14 | Diagram Family: C4 |
| BlockBeta full support | bd-hudcn.1.15 | Diagram Type: block-beta |
| PacketBeta full support | bd-hudcn.1.16 | Diagram Type: packet-beta |
| ArchitectureBeta full support | bd-hudcn.1.17 | Diagram Type: architecture-beta |
| Matrix/fallback refresh | bd-hudcn.1.3 | Compatibility matrix + fallback policy refresh |
| Gantt IR fix | (needs bead or fold into bd-hudcn.1.3) | Gantt IR normalization gap |
| Sequence enhancements | (needs bead or fold into existing) | Sequence participant/fragment support |
| Class UML rendering | (needs bead or fold into existing) | Class diagram UML box renderer |
| State shapes | (needs bead or fold into existing) | State diagram visual fidelity |
| Mindmap shapes | (needs bead or fold into existing) | Mindmap node shape variety |
| Pie matrix upgrade | (needs bead or fold into bd-hudcn.1.3) | Pie: Partial → Supported |
