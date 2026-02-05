# Mermaid Engine Config

This document defines the deterministic configuration surface for the Mermaid
terminal diagram engine.

## Goals

- Make Mermaid rendering controllable via a single config struct.
- Provide explicit, deterministic environment overrides (no hidden heuristics).
- Validate config early with clear errors.

## Configuration (MermaidConfig)

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | Master enable/disable switch. |
| `glyph_mode` | enum | `unicode` | `unicode` or `ascii`. |
| `tier_override` | enum | `auto` | `compact`, `normal`, `rich`, or `auto`. |
| `max_nodes` | usize | `200` | Hard cap on node count. |
| `max_edges` | usize | `400` | Hard cap on edge count. |
| `route_budget` | usize | `4000` | Routing work budget (units: ops). |
| `layout_iteration_budget` | usize | `200` | Max layout iterations. |
| `max_label_chars` | usize | `48` | Maximum characters per label (pre-wrap). |
| `max_label_lines` | usize | `3` | Maximum wrapped lines per label. |
| `wrap_mode` | enum | `wordchar` | `none`, `word`, `char`, `wordchar`. |
| `enable_styles` | bool | `true` | Allow Mermaid `classDef`/`style` paths. |
| `enable_init_directives` | bool | `false` | Allow `%%{init: ...}%%` directives. |
| `enable_links` | bool | `false` | Enable link rendering. |
| `link_mode` | enum | `off` | `inline`, `footnote`, `off`. Requires `enable_links=true` unless `off`. |
| `sanitize_mode` | enum | `strict` | `strict` or `lenient`. |
| `error_mode` | enum | `panel` | `panel`, `raw`, `both`. |
| `log_path` | Option<String> | `None` | JSONL error/diagnostic output. |
| `cache_enabled` | bool | `true` | Enable diagram cache. |
| `capability_profile` | Option<String> | `None` | Override terminal capability profile. |

## Environment Variables

All env vars use the `FTUI_MERMAID_*` prefix:

- `FTUI_MERMAID_ENABLE` (bool)
- `FTUI_MERMAID_GLYPH_MODE` = `unicode` | `ascii`
- `FTUI_MERMAID_TIER` = `compact` | `normal` | `rich` | `auto`
- `FTUI_MERMAID_MAX_NODES` (usize)
- `FTUI_MERMAID_MAX_EDGES` (usize)
- `FTUI_MERMAID_ROUTE_BUDGET` (usize)
- `FTUI_MERMAID_LAYOUT_ITER_BUDGET` (usize)
- `FTUI_MERMAID_MAX_LABEL_CHARS` (usize)
- `FTUI_MERMAID_MAX_LABEL_LINES` (usize)
- `FTUI_MERMAID_WRAP_MODE` = `none` | `word` | `char` | `wordchar`
- `FTUI_MERMAID_ENABLE_STYLES` (bool)
- `FTUI_MERMAID_ENABLE_INIT_DIRECTIVES` (bool)
- `FTUI_MERMAID_ENABLE_LINKS` (bool)
- `FTUI_MERMAID_LINK_MODE` = `inline` | `footnote` | `off`
- `FTUI_MERMAID_SANITIZE_MODE` = `strict` | `lenient`
- `FTUI_MERMAID_ERROR_MODE` = `panel` | `raw` | `both`
- `FTUI_MERMAID_LOG_PATH` (string path)
- `FTUI_MERMAID_CACHE_ENABLED` (bool)
- `FTUI_MERMAID_CAPS_PROFILE` (string)
- `FTUI_MERMAID_CAPABILITY_PROFILE` (string, alias)

Note: `capability_profile` is parsed and stored for determinism, but currently
serves as a reserved override (no behavioral change yet).

## Determinism

- Environment overrides are parsed deterministically at runtime.
- Invalid values are reported as structured `MermaidConfigError`s.
- Rendering behavior must not depend on wall-clock time or non-deterministic IO.

## Engine Pipeline (Current)

The Mermaid engine is split into deterministic phases:

1. **Parse + diagnostics** (`parse_with_diagnostics`, `prepare_with_policy`).
2. **Init directives** → `MermaidInitConfig`, `theme_overrides`, `init_config_hash`.
3. **Compatibility + validation** → warnings/errors with spans.
4. **Normalize** → `MermaidDiagramIr` (`normalize_ast_to_ir`) + guard/degradation plan.
5. **Layout** → `DiagramLayout` (`layout_diagram` in `mermaid_layout`).
6. **Render** → `Buffer` (`MermaidRenderer::render` in `mermaid_render`).

When `log_path` is set, the engine appends JSONL evidence events
(`mermaid_prepare`, `mermaid_guard`, `mermaid_links`).

## Diagram IR + Normalization

The AST is syntax-focused. The engine normalizes into a semantic IR with
stable, deterministic ordering so semantically equivalent inputs produce
identical IR.

### IR Core (Diagram-Agnostic)

- `DiagramIr { diagram_type, direction, nodes, edges, clusters, labels, ports, style_refs, meta }`
- `DiagramMeta { diagram_type, direction, support_level, init, theme_overrides, guard }`
- `IrNode { id, label, classes, style_ref, span_primary, span_all, implicit }`
- `IrEdge { from: Endpoint, to: Endpoint, arrow, label, style_ref, span }`
- `Endpoint = Node(NodeId) | Port(PortId)`
- `IrPort { node, name, side_hint, span }`
- `IrCluster { id, title, members, span }`

Typed sub-IRs (sequence lifelines, gantt timelines, etc.) can hang off `DiagramIr`
as optional, diagram-specific payloads.

### Normalization Rules (Deterministic)

1. Resolve `direction` with precedence: init-directive override → header → default `TB`.
2. Apply init directives into `DiagramMeta` before any style resolution.
3. Deduplicate nodes by normalized id; stable ordering by `(id, first_span.line, first_span.col, insertion_idx)`.
4. Edges referencing missing nodes create implicit nodes (emit warning).
5. Parse endpoint ports (`node:port`) into `IrPort`, and attach a default `side_hint`
   based on direction (TB → top/bottom, LR → left/right).
6. Build `IrCluster` membership from `subgraph` blocks with stable ordering.
7. Preserve class/style/link directives as raw references; resolve via `resolve_styles`
   with deterministic precedence and warnings (see below).
8. Validate malformed ids/edges/ports and emit deterministic warnings with spans.

## Layout + Routing (Implemented)

Layout is deterministic and lives in `crates/ftui-extras/src/mermaid_layout.rs`.
The engine uses a Sugiyama-style layered layout:

- Rank assignment (longest-path from sources).
- Ordering within ranks (barycenter crossing minimization).
- Coordinate assignment (compact placement with spacing).
- Cluster boundary computation.
- Edge routing via waypoint paths.

Output is in abstract world units (`DiagramLayout`) and includes `LayoutStats`
such as crossings, rank count, total bends, and iteration/budget usage.

## Renderer (Implemented)

The terminal renderer in `crates/ftui-extras/src/mermaid_render.rs` maps the
world-space layout into a `Buffer` and supports:

- Unicode box-drawing glyphs with ASCII fallback (`MermaidGlyphMode`).
- Render order: clusters → edges → nodes → labels.
- Arrowheads, label truncation, and clipped text drawing.
- Viewport fitting to the target `Rect` with a 1-cell margin.

Current limitations:
- Line styles are solid (dash/dot glyphs are reserved but not yet wired).
- Diagonal segments are approximated with L-shaped bends.
- Styling is minimal (palette-based colors; no per-edge style glyphs yet).

## Scale Adaptation + Fidelity Tiers

`MermaidTier` maps to `MermaidFidelity` (`rich`, `normal`, `compact`, `outline`).
Guardrails (limits + budgets) may degrade fidelity deterministically:

- **hide_labels** (nodes/edges/clusters).
- **collapse_clusters** (remove cluster boxes).
- **simplify_routing** (reduce route complexity).
- **reduce_decoration** (drop class/style decoration).
- **force_glyph_mode=ascii** in `outline`.

The degradation plan is recorded in `MermaidGuardReport` and emitted to JSONL
when `log_path` is configured.

## Validation Rules

- `max_nodes`, `max_edges`, `route_budget`, `layout_iteration_budget`,
  `max_label_chars`, and `max_label_lines` must be >= 1.
- If `enable_links=false`, `link_mode` must be `off`.

## Style Resolution (Implemented)

Style directives (`classDef`, `class`, `style`, `linkStyle`) are parsed into
structured properties and resolved deterministically:

**Supported properties**
- `fill`, `background`, `background-color`
- `stroke`, `border-color`
- `stroke-width`, `border-width` (px allowed)
- `stroke-dasharray`
- `color`, `font-color`
- `font-weight`

Unsupported keys are recorded as `mermaid/unsupported/style` warnings with spans.

**Precedence (last wins)**
1. `themeVariables` defaults (if present)
2. `classDef` styles (merged in class list order)
3. Node-specific `style`

`linkStyle default` applies to all edges; `linkStyle <idx>` applies by edge
index in **source order** (edges preserve statement order to keep indices stable).

**Theme variables mapped**
- `primaryColor` → `fill`
- `primaryTextColor` → `color`
- `primaryBorderColor` → `stroke`

**Contrast clamp**
If both `fill` and `color` are set, a minimum contrast ratio is enforced
via `clamp_contrast` (currently 3.0). When clamped, the source list records
`contrast-clamp`.

## Init Directives (Supported Subset)

When `enable_init_directives=true`, `%%{init: {...}}%%` blocks are parsed into a
small, deterministic subset and then merged (last directive wins):

Supported keys:
- `theme` (string) — mapped to Mermaid theme id.
- `themeVariables` (object) — string/number/bool values only.
- `flowchart.direction` (string) — one of `TB`, `TD`, `LR`, `RL`, `BT`.

Unsupported keys or invalid types are ignored with
`mermaid/unsupported/directive` warnings. If `enable_init_directives=false`,
init directives are ignored with the same warning.

## Compatibility Matrix (Current)

Parser + normalization are implemented for all listed types. Layout + rendering
are currently graph-oriented (nodes/edges) and should be treated as **partial**
outside flowchart/graph use cases.

| Diagram Type | Support | Notes |
| --- | --- | --- |
| Graph / Flowchart | partial | Layout + renderer implemented (basic glyphs, labels, clusters). |
| Sequence | partial | Parsed into AST; render path pending. |
| State | partial | Parsed into AST; render path pending. |
| Gantt | partial | Parsed into AST; render path pending. |
| Class | partial | Parsed into AST; render path pending. |
| ER | partial | Parsed into AST; render path pending. |
| Mindmap | partial | Parsed into AST; render path pending. |
| Pie | partial | Parsed into AST; render path pending. |
| Unknown | Unsupported | Deterministic fallback (see below). |

If a diagram type is **unsupported**, the fallback policy is to show an error
panel (fatal compatibility report).

## Warning Codes (Fallback Policy)

Warnings are deterministic and use stable codes:

- `mermaid/unsupported/diagram` — diagram type not supported
- `mermaid/unsupported/directive` — init/raw directive ignored
- `mermaid/unsupported/style` — style/class directives ignored
- `mermaid/unsupported/link` — links ignored
- `mermaid/unsupported/feature` — unknown statement ignored
- `mermaid/sanitized/input` — input sanitized (strict mode)
- `mermaid/limit/exceeded` — `max_nodes`/`max_edges`/label limits triggered
- `mermaid/budget/exceeded` — route/layout budget exhausted

## Compatibility Matrix

The parser is intentionally minimal and deterministic. Supported headers are:

| Diagram Type | Header Keywords | Support | Notes |
| --- | --- | --- | --- |
| Flowchart / Graph | `graph`, `flowchart` | Supported | Nodes + edges, optional subgraphs, explicit direction. |
| Sequence | `sequenceDiagram` | Partial | Basic messages only; no `alt`, `opt`, `loop`, activation boxes, or notes. |
| State | `stateDiagram` | Partial | Simple transitions; nested/composite states not guaranteed. |
| Gantt | `gantt` | Partial | Simple task lines; no date math or complex sections. |
| Class | `classDiagram` | Partial | Basic members; generics/annotations may be dropped. |
| ER | `erDiagram` | Partial | Basic relationships; advanced cardinalities may degrade. |
| Mindmap | `mindmap` | Partial | Depth indentation only; icons/markup not guaranteed. |
| Pie | `pie` | Partial | Label/value entries only. |
| Unknown | other | Unsupported | Deterministic fallback (see below). |

Mermaid constructs outside this subset must degrade deterministically and emit
warnings instead of failing or producing unstable output.

## Fallback Policy (Deterministic)

When encountering unsupported input, the engine degrades in a predictable order:

1. **Diagram type unknown** → emit `mermaid/unsupported/diagram` and render an
   error panel (or raw fenced text if `error_mode=raw`).
2. **Config disabled** → render a disabled panel with a single‑line summary.
3. **Unsupported statements** (e.g., advanced directives) → ignore and emit
   `mermaid/unsupported/feature` with span.
4. **Limits exceeded** (`max_nodes`, `max_edges`, label limits) → clamp and emit
   `mermaid/limit/exceeded` with counts.
5. **Budget exceeded** (`route_budget`, `layout_iteration_budget`) → degrade
   tier `rich → normal → compact → outline`, emitting `mermaid/budget/exceeded`.
6. **Security violations** (HTML/JS, unsafe links) → strip and emit
   `mermaid/sanitized/input`.

Implementation note:
- `ftui_extras::mermaid::validate_ast` applies the compatibility matrix plus
  `MermaidFallbackPolicy` to emit deterministic warnings/errors before layout.

The **outline** fallback is the lowest fidelity tier: labels are hidden, clusters
are collapsed, decoration is reduced, and glyphs are forced to ASCII. Rendering
still uses the deterministic layout/renderer pipeline.

## Complexity Guards + Degradation

Guard checks run after IR normalization and are deterministic:

- **Complexity score** = `nodes + edges + labels + clusters` (counts stored in meta).
- **Label limits** clamp to `max_label_chars` and `max_label_lines` (counts recorded).
- **Budget estimates** are heuristic but deterministic (route ops + layout iterations).
- **Degradation plan** may hide labels, collapse clusters, simplify routing,
  reduce decoration, and force ASCII fallback depending on which limits/budgets
  are exceeded.

## Warning Taxonomy (JSONL + Panels)

Warnings are structured and deterministic, including `code`, `message`,
`diagram_type`, and `span` (line/col). Implemented codes:

| Code | When | Severity |
| --- | --- | --- |
| `mermaid/unsupported/diagram` | Header not recognized | error |
| `mermaid/unsupported/directive` | `%%{init}%%` or raw directive blocked | warn |
| `mermaid/unsupported/style` | `classDef`/`style`/`linkStyle` blocked | warn |
| `mermaid/unsupported/link` | link/click disabled or sanitized | warn |
| `mermaid/unsupported/feature` | Statement unsupported in current diagram | warn |
| `mermaid/sanitized/input` | HTML/JS/unsafe text stripped | warn |
| `mermaid/limit/exceeded` | `max_nodes`/`max_edges`/label limits triggered | warn |
| `mermaid/budget/exceeded` | Route/layout budget exhausted | warn |

Reserved for upcoming guard/degradation phases (not yet emitted):

| Code | When | Severity |
| --- | --- | --- |
| `mermaid/disabled` | Config `enabled=false` | info |
| `mermaid/parse/error` | Syntax error with span | error |

## JSONL Evidence Logs

If `log_path` is set, the engine emits deterministic JSONL events:

- `mermaid_prepare`
  - `diagram_type`, `init_config_hash`, `init_theme`, `init_theme_vars`
  - `warnings`, `errors`
- `mermaid_guard`
  - `diagram_type`
  - `complexity` (nodes/edges/labels/clusters/ports/style_refs/score)
  - `label_limits` (over_chars, over_lines)
  - `budget_estimates` (route_ops, layout_iterations)
  - `guard_codes` (e.g., `mermaid/limit/exceeded`, `mermaid/budget/exceeded`)
  - `degradation` (target_fidelity, hide_labels, collapse_clusters, simplify_routing,
    reduce_decoration, force_glyph_mode)
- `mermaid_links`
  - `link_mode`, `total_count`, `allowed_count`, `blocked_count`

## Security Policy

- **No HTML/JS execution**, ever. HTML is stripped or treated as literal text.
- **No external fetches** or URL resolution during rendering.
- Links are sanitized and only emitted if `enable_links=true` and
  `sanitize_mode` allows them.
- All decisions must be logged deterministically with spans.

## Hyperlink Policy

Link directives are sanitized via `sanitize_url`:

- **Blocked protocols** (always rejected): `javascript:`, `vbscript:`, `data:`,
  `file:`, `blob:`.
- **Strict mode** allows only: `http:`, `https:`, `mailto:`, `tel:` plus
  relative paths (no protocol prefix).
- **Lenient mode** allows any protocol not in the blocked list.

Blocked links emit `mermaid/sanitized/input` warnings and are excluded from
link resolution metrics.

## Debug Overlay

The demo debug overlay renders a one-line Mermaid summary so active
configuration is visible during interactive runs.

## Usage (Current API)

```rust
use ftui_extras::mermaid::{
    MermaidConfig, MermaidCompatibilityMatrix, MermaidFallbackPolicy, prepare, normalize_ast_to_ir,
};
use ftui_extras::mermaid_layout::layout_diagram;
use ftui_extras::mermaid_render::MermaidRenderer;
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;

let source = "graph TD; A-->B";
let config = MermaidConfig::default();
let matrix = MermaidCompatibilityMatrix::default();

let prepared = prepare(source, &config, &matrix);
let ir_parse = normalize_ast_to_ir(
    &prepared.ast,
    &config,
    &matrix,
    &MermaidFallbackPolicy::default(),
);

if ir_parse.errors.is_empty() {
    let layout = layout_diagram(&ir_parse.ir, &config);
    let renderer = MermaidRenderer::new(&config);
    let mut buffer = Buffer::new(80, 24);
    renderer.render(&layout, &ir_parse.ir, Rect::new(0, 0, 80, 24), &mut buffer);
}
```

## Troubleshooting Width + Density

- **Text too wide**: lower `max_label_chars`, `max_label_lines`, or set
  `wrap_mode=wordchar`.
- **Crowding**: set `tier_override=compact` or `outline`.
- **Narrow terminals**: switch to `glyph_mode=ascii` to reduce width surprises.
- **Diagnostics**: enable `FTUI_MERMAID_LOG_PATH` to capture guard + link events.
