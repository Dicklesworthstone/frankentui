# Mermaid Coverage Gap Ledger

Bead: `bd-hudcn.1.2`
Last updated: 2026-02-06
Baseline: Mermaid `11.4.0` (`MERMAID_BASELINE_VERSION`)
Source of truth: `DIAGRAM_FAMILY_REGISTRY` + `MermaidCompatibilityMatrix::default()` (`crates/ftui-extras/src/mermaid.rs`)

This is an audit artifact: for each canonical diagram family, it records:
- current support level
- which repo surfaces exercise it (demo/markdown/fixtures)
- what is missing
- the single bead that owns closing the gap

## Pipeline / Layers

| Layer | Meaning |
|---|---|
| Header | `parse_header()` selects `DiagramType` |
| Parser | `parse_with_diagnostics()` emits typed `Statement` variants or `Statement::Raw` |
| IR | `normalize_ast_to_ir()` builds `MermaidDiagramIr` and emits warnings/errors |
| Layout | `mermaid_layout::layout_diagram_with_spacing()` produces `DiagramLayout` |
| Render | `mermaid_render::render_diagram_adaptive()` + `MermaidRenderer` draw into `Buffer` |
| Matrix | `MermaidCompatibilityMatrix::default()` drives deterministic fallback behavior |
| Fixture file | `.mmd` exists in `crates/ftui-extras/tests/fixtures/mermaid/` |
| Fixture wired | fixture is enumerated in `crates/ftui-extras/tests/mermaid_fixtures.rs` |
| Demo (showcase) | sample exists in `crates/ftui-demo-showcase/src/screens/mermaid_showcase.rs` |
| Demo (mega) | sample exists in `crates/ftui-demo-showcase/src/screens/mermaid_mega_showcase.rs` |
| Markdown fence | representative ` ```mermaid ` fence exists in tests/demo content |

## Canonical Families (v11.4.0)

Supported (2): Graph, ER

Partial (9):
- Sequence
- State
- Gantt
- Class
- Mindmap
- Pie
- GitGraph
- Journey
- Requirement

Unsupported (12):
- Timeline
- QuadrantChart
- XyChart
- Sankey
- BlockBeta
- PacketBeta
- ArchitectureBeta
- C4Context
- C4Container
- C4Component
- C4Dynamic
- C4Deployment

## Gap Ledger (1:1 mapping to canonical family IDs)

Surfaces legend:
- `F` = fixture file exists
- `W` = fixture wired in `mermaid_fixtures.rs`
- `S` = demo showcase preset exists
- `M` = demo mega sample exists
- `MD` = markdown fence sample exists

### Partial Families (9)

| Family (keyword) | Surfaces | Current gap summary | Root cause class | Owning bead |
|---|---|---|---|---|
| Sequence (`sequenceDiagram`) | F,W,S,M | Partial slice; missing sequence-native features (participants/fragments/activations/notes) and coverage | RC-PARTIAL-PIPELINE | `bd-2kn9a` |
| State (`stateDiagram-v2`) | F,W,S,M | Implementation exists; remaining work is coverage wiring (fixtures/snapshots/e2e/demo parity) | RC-COVERAGE-WIRING | `bd-hudcn.1.4` |
| Gantt (`gantt`) | F,W,S,M | Parser emits typed gantt statements, but IR normalization ignores them -> no meaningful layout/render | RC-IR-MISSING | `bd-30t8a` |
| Class (`classDiagram`) | F,W,S,M | Renders as generic graph; missing UML class boxes/compartments and annotations | RC-RENDER-TODO | `bd-2d9fm` |
| Mindmap (`mindmap`) | F,W,S,M | Layout/render fidelity still partial (shape variety, polish) plus coverage | RC-PARTIAL-PIPELINE | `bd-9ta1z` |
| Pie (`pie`) | F,W,S,M | Pie renders (including `showData`), but support-level/coverage metadata needs alignment and gating | RC-POLICY-DRIFT | `bd-hudcn.1.3` |
| GitGraph (`gitGraph`) | F,S | Partial slice (lane layout/render exists); needs completeness + coverage + demo parity | RC-PARTIAL-PIPELINE | `bd-hudcn.1.7` |
| Journey (`journey`) | F,S,M | Parser+IR exist; missing journey-native layout/render (scores, actor lanes) + coverage | RC-LAYOUT-RENDER-TODO | `bd-hudcn.1.8` |
| Requirement (`requirementDiagram`) | F,S | Parser+IR exist; missing requirement-native render (badges/metadata) + coverage | RC-LAYOUT-RENDER-TODO | `bd-hudcn.1.9` |

### Unsupported Families (12)

For these families today:
- Matrix is `Unsupported` -> deterministic error `mermaid/unsupported/diagram`
- Parser routes body to `Statement::Raw` -> warnings `mermaid/unsupported/feature`

| Family (keyword) | Surfaces | Unsupported behavior summary | Root cause class | Owning bead |
|---|---|---|---|---|
| Timeline (`timeline`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.10` |
| QuadrantChart (`quadrantChart`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.11` |
| XyChart (`xychart-beta`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.12` |
| Sankey (`sankey-beta`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.13` |
| BlockBeta (`block-beta`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.15` |
| PacketBeta (`packet-beta`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.16` |
| ArchitectureBeta (`architecture-beta`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.17` |
| C4Context (`C4Context`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.14` |
| C4Container (`C4Container`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.14` |
| C4Component (`C4Component`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.14` |
| C4Dynamic (`C4Dynamic`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.14` |
| C4Deployment (`C4Deployment`) | F | Header recognized; body raw-only; no typed IR/layout/render | RC-RAW-DISPATCH | `bd-hudcn.1.14` |

## Cross-surface Coverage Gaps

| Surface | Current state | Gap | Primary bead |
|---|---|---|---|
| Markdown fences | Mermaid fences in code/tests are graph-centric | Need representative fences (or explicit policy) per canonical family for reproducible markdown coverage | `bd-hudcn.1.6` |
| Fixture harness wiring | Many fixture files exist, but `mermaid_fixtures.rs` enumerates a small legacy subset | Wire full canonical corpus (or add metadata-driven harness) so evidence is reproducible | `bd-hudcn.1.4` |
| Demo parity | Demo presets do not comprehensively represent canonical families (especially unsupported ones) | Demo surfaces should reflect registry status and provide deterministic repro paths | `bd-hudcn.1.5` |

## Root Cause Classes (vocabulary)

| Code | Definition |
|---|---|
| RC-RAW-DISPATCH | Header recognized, but content stays `Statement::Raw` (no typed pipeline) |
| RC-IR-MISSING | Parser emits typed statements, but IR normalization ignores them |
| RC-RENDER-TODO | IR/layout exist but renderer lacks family-native representation |
| RC-LAYOUT-RENDER-TODO | Parser/IR exist; family-native layout/render are missing (generic fallback only) |
| RC-PARTIAL-PIPELINE | Partial implementation slice exists across stages; needs completeness + coverage |
| RC-COVERAGE-WIRING | Implementation exists; remaining work is fixtures/snapshots/e2e/demo parity gating |
| RC-POLICY-DRIFT | Code behavior and registry/matrix/feature-matrix metadata are out of sync |

## Ranked Implementation Order

| Rank | Bead | Why |
|---:|---|---|
| 1 | `bd-30t8a` | Gantt IR gap blocks everything; demo looks blank even though parser works. |
| 2 | `bd-hudcn.1.7` | gitGraph is high-visibility; lane slice exists but needs completeness + parity. |
| 3 | `bd-hudcn.1.8` | journey is common; needs journey-native layout/render. |
| 4 | `bd-hudcn.1.9` | requirementDiagram needs requirement-native render + parity. |
| 5 | `bd-2kn9a` | sequence is high-impact but needs a bigger feature slice. |
| 6 | `bd-2d9fm` | class diagrams need UML boxes to stop looking like generic graphs. |
| 7 | `bd-9ta1z` | mindmap fidelity + coverage. |
| 8 | `bd-hudcn.1.14` | C4 is valuable but heavy; shared infra across 5 variants. |
| 9 | `bd-hudcn.1.10` | timeline is useful and currently raw-only. |
| 10 | `bd-hudcn.1.4` / `bd-hudcn.1.5` / `bd-hudcn.1.6` | fixtures -> demo parity -> CI gate (locks in all progress). |
