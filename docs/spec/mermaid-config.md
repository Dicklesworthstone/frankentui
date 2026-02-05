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

## Determinism

- Environment overrides are parsed deterministically at runtime.
- Invalid values are reported as structured `MermaidConfigError`s.
- Rendering behavior must not depend on wall-clock time or non-deterministic IO.

## Diagram IR + Normalization (Planned)

The AST is syntax-focused. The renderer/layout stack needs a semantic IR with
stable, deterministic normalization so semantically equivalent inputs produce
identical IR.

### IR Core (Diagram-Agnostic)

- `DiagramIr { diagram_type, direction, nodes, edges, clusters, labels, ports, style_refs, meta }`
- `DiagramMeta { init: MermaidInitParse, warnings, source_spans, diagram_flags }`
- `IrNode { id, label, classes, style, span_primary, span_all }`
- `IrEdge { from: Endpoint, to: Endpoint, arrow, label, classes, style, span }`
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
7. Preserve class/style/link directives as raw references; resolution and palette mapping
   occur later (see styling tasks).
8. Validate malformed ids/edges/ports and emit deterministic warnings with spans.

## Validation Rules

- `max_nodes`, `max_edges`, `route_budget`, `layout_iteration_budget`,
  `max_label_chars`, and `max_label_lines` must be >= 1.
- If `enable_links=false`, `link_mode` must be `off`.

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

## Compatibility Matrix (Parser-Only)

Current engine status is **parser‑only** for all diagram types. Rendering is
expected to be **partial** until the TME renderer is complete.

| Diagram Type | Support | Notes |
| --- | --- | --- |
| Graph / Flowchart | partial | Parsed into AST; renderer pending |
| Sequence | partial | Parsed into AST; renderer pending |
| State | partial | Parsed into AST; renderer pending |
| Gantt | partial | Parsed into AST; renderer pending |
| Class | partial | Parsed into AST; renderer pending |
| ER | partial | Parsed into AST; renderer pending |
| Mindmap | partial | Parsed into AST; renderer pending |
| Pie | partial | Parsed into AST; renderer pending |

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

1. **Diagram type unknown** → emit `MERMAID_UNSUPPORTED_DIAGRAM` and render an
   error panel (or raw fenced text if `error_mode=raw`).
2. **Config disabled** → render a disabled panel with a single‑line summary.
3. **Unsupported statements** (e.g., advanced directives) → ignore and emit
   `MERMAID_UNSUPPORTED_TOKEN` with span.
4. **Limits exceeded** (`max_nodes`, `max_edges`, label limits) → clamp and emit
   `mermaid/limit/exceeded` with counts.
5. **Budget exceeded** (`route_budget`, `layout_iteration_budget`) → degrade
   tier `rich → normal → compact → outline`, emitting `mermaid/budget/exceeded`.
6. **Security violations** (HTML/JS, unsafe links) → strip and emit
   `mermaid/sanitized/input`.

Implementation note:
- `ftui_extras::mermaid::validate_ast` applies the compatibility matrix plus
  `MermaidFallbackPolicy` to emit deterministic warnings/errors before layout.

The **outline** fallback renders a deterministic, sorted list of nodes and
edges (stable ordering by insertion + lexicographic tie‑break).

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

## Security Policy

- **No HTML/JS execution**, ever. HTML is stripped or treated as literal text.
- **No external fetches** or URL resolution during rendering.
- Links are sanitized and only emitted if `enable_links=true` and
  `sanitize_mode` allows them.
- All decisions must be logged deterministically with spans.

## Debug Overlay

The demo debug overlay renders a one-line Mermaid summary so active
configuration is visible during interactive runs.
