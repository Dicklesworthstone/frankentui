# Mermaid Showcase Screen - Spec + UX Flow

This spec defines the UX, interaction model, layout behavior, and state model for the
**Mermaid Showcase** demo screen (bd-2nkmi.1).

---

## 1) Goals
- Present Mermaid terminal rendering as a first-class, interactive demo.
- Make it obvious how samples, layout, fidelity, and performance relate.
- Provide deterministic, keyboard-first exploration with clear status feedback.
- Keep the screen implementable from this document without hunting prior context.

## 2) Non-Goals
- Full Mermaid authoring/editor experience (read-only samples for now).
- Runtime plugin system for external Mermaid sources.
- Mouse-driven interaction as a requirement (keyboard remains primary).

---

## 3) Screen Regions + Information Hierarchy

### 3.1 Header Bar (Top, 1 row)
Purpose: immediate orientation and status.

Fields (left → right):
- Screen title: `Mermaid Showcase`
- Active sample name + index: `Sample: Flow-01 (12/68)`
- Layout mode: `Layout: Auto` / `Layout: Dense` / `Layout: Spacious`
- Fidelity tier: `Tier: Auto` / `Rich` / `Normal` / `Compact` / `Outline`
- Status badge: `OK`, `WARN`, `ERR`

### 3.2 Sample Library (Primary navigation)
Purpose: choose what is rendered.

Contents:
- Scrollable list of samples grouped by category (e.g., Flow, Sequence, Class, State, ER, Gantt, Mindmap, EdgeCases).
- Each item shows:
  - Sample name (short)
  - Tags (small: `dense`, `labels`, `links`, `cluster`, `stress`)
  - Complexity hint (S/M/L or node/edge counts)

### 3.3 Render Viewport (Primary focus)
Purpose: show the actual diagram.

Content:
- Rendered Mermaid diagram in a framed viewport.
- Optional overlay line: `Nodes: 42  Edges: 58  Size: 94x22`
- If rendering fails, show fallback per error policy + short error line.

### 3.4 Control Panel (Secondary controls)
Purpose: reveal controllable axes and active toggles.

Suggested sections:
- **Layout**: mode, iteration budget, route budget
- **Fidelity**: tier override, glyph mode, wrap mode
- **Style**: class/style enable, link mode
- **Viewport**: zoom, fit, pan hints

### 3.5 Metrics Panel (Performance + diagnostics)
Purpose: show cost and quality signals.

Metrics (required):
- Parse time (ms)
- Layout time (ms)
- Render time (ms)
- Layout iterations
- Objective score (if available)
- Constraint violations count
- Fallback tier / degradation reason (if any)

---

## 4) Layout Behavior by Terminal Size

### 4.1 80x24 (Minimum viable)
Layout intent: maximize viewport, keep navigation possible.

```
[Header: 1 row]
[Sample bar: 2 rows]  (active sample + quick prev/next hints)
[Viewport: 18 rows]
[Footer: 3 rows]      (key hints + compact metrics line if enabled)
```

Rules:
- Sample list collapses to a single-line selector (prev/next only).
- Metrics panel defaults to collapsed; `m` toggles a single-line summary.
- Control panel hidden; `c` toggles a 6-row overlay (temporary).

### 4.2 120x40 (Default demo target)
Layout intent: clear separation of nav / render / controls.

```
Columns: 26 | 66 | 28
Rows:    1 header + body + 1 footer

Left:   Sample list (full)
Center: Render viewport (dominant)
Right:  Control panel (top) + Metrics panel (bottom)
Footer: Key hints (1 row)
```

Rules:
- Metrics panel occupies ~12 rows at bottom-right.
- Control panel uses remaining right column space.
- Viewport keeps at least 60% width.

### 4.3 200x60 (Large showcase / lab mode)
Layout intent: exhaustive information + comparison-friendly.

```
Columns: 34 | 116 | 50
Rows:    1 header + body + 1 footer

Left:   Sample list (full + categories visible)
Center: Render viewport (large)
Right:  Split stack: Control panel (top) + Metrics (middle) + Status log (bottom)
```

Rules:
- Metrics panel expanded (>= 18 rows).
- Optional mini log panel for last 8 events (layout/route warnings).

### 4.4 Status Log Panel (Spec + Schema)
Purpose: deterministic, testable summary of recent activity and warnings.

Placement rules:
- Render only at 200x60 or when explicitly toggled on.
- Default height: 8 rows (one row per event).
- If space constrained, truncate oldest events first.

Event types (fixed set):
- `render_start`
- `render_done`
- `layout_warning`
- `route_warning`
- `fallback_used`
- `error`

Schema fields (per entry):
- `schema_version` (string, e.g. `mermaid-statuslog-v1`)
- `ts_ms` (u64, monotonic ms from run start)
- `sample` (string, sample id/name)
- `mode` (string, `inline` or `altscreen`)
- `dims` (string, `COLSxROWS`)
- `layout_mode` (string)
- `fidelity` (string)
- `event` (string, from the fixed set)
- `status` (string, `ok|warn|error`)
- `message` (string, short human summary)

Update cadence:
- Append one entry per render step and warning.
- Never reorder; stable ordering for snapshot/E2E validation.

---

## 5) Interaction Model + Keybindings

### 5.1 Sample Navigation
- `Up` / `Down` or `j` / `k` : previous/next sample
- `Home` / `End` : first/last sample
- `Enter` : re-render current sample (explicit refresh)

### 5.2 Size + Zoom + Fit
- `+` / `-` : adjust viewport zoom step (10% increments)
- `0` : reset zoom to 100%
- `f` : fit diagram to viewport (auto zoom)

### 5.3 Layout Control
- `l` : toggle layout mode (Auto → Dense → Spacious → Auto)
- `r` : force re-layout (clear caches and recompute)

### 5.4 Metrics + Panels
- `m` : toggle metrics panel (collapsed/expanded)
- `c` : toggle control panel (collapsed/expanded)

### 5.5 Fidelity + Styles
- `t` : cycle fidelity tier (Auto → Rich → Normal → Compact → Outline → Auto)
- `g` : toggle glyph mode (Unicode ↔ ASCII)
- `s` : toggle Mermaid styles (classDef/style)
- `w` : cycle wrap mode (Word → Char → WordChar)

### 5.6 Global
- `?` : help overlay (keybindings summary)
- `Esc` : close overlay / collapse panels

---

## 6) State Model (Implementation Outline)

```text
MermaidShowcaseState
  samples: Vec<SampleDef>
  selected_index: usize
  filter: Option<String>
  layout_mode: LayoutMode (Auto | Dense | Spacious)
  fidelity: MermaidTier (Auto | Rich | Normal | Compact | Outline)
  glyph_mode: MermaidGlyphMode (Unicode | Ascii)
  wrap_mode: MermaidWrapMode (Word | Char | WordChar)
  styles_enabled: bool
  metrics_visible: bool
  controls_visible: bool
  viewport_zoom: f32
  viewport_pan: (i16, i16)
  last_render: MermaidRenderReport
  last_error: Option<MermaidError>
```

Command flow:
- Input → `MermaidShowcaseMsg` → update state → (re-render?) → view.
- Any changes to layout/fidelity/sample trigger a re-render.
- Zoom/pan updates viewport transform only; re-render optional if draw is cached.

---

## 7) Metrics Definitions (Required Fields)
- `parse_ms` : time to parse Mermaid source to AST/IR.
- `layout_ms` : time to compute `DiagramLayout`.
- `render_ms` : time to render into `Buffer`.
- `layout_iterations` : iterations used by layout solver.
- `objective_score` : optional cost score if solver reports it.
- `constraint_violations` : count of violated layout constraints.
- `fallback_tier` : which tier was used if degraded (or `none`).
- `fallback_reason` : short string describing why degradation occurred.

---

## 8) Visual Composition Notes
- Viewport must be the dominant visual region at all sizes.
- Sample list should always remain discoverable, even if collapsed.

---

## 9) Sample Library Catalog (bd-2nkmi.2)

Flow (S/M/L)
- Flow Basic — branch + decision
- Flow Subgraphs — clustered groups + edge labels
- Flow Dense — many nodes/edges, crossings
- Flow Long Labels — wrapping stress
- Flow Unicode — non-ASCII labels
- Flow Styles — classDef/style directives

Sequence (S/M/L)
- Sequence Mini — basic request/response
- Sequence Checkout — multi-hop API flow
- Sequence Dense — tight spacing, repeated hops

Class (S/M)
- Class Basic — inheritance + association
- Class Members — fields/methods

State (S/M)
- State Basic — start/end tokens
- State Composite — nested state block + note

ER (M)
- ER Basic — cardinality + relationship labels

Gantt (M)
- Gantt Basic — title, sections, tasks

Mindmap (S/L)
- Mindmap Seed — minimal tree
- Mindmap Deep — multi-level roadmap

Pie (S/M)
- Pie Basic — title + showData
- Pie Many — many slices

Unsupported (kept for coverage + future support)
- Gitgraph Basic — gitGraph
- Journey Basic — journey
- Requirement Basic — requirementDiagram
- Use clear semantic colors: status (OK/WARN/ERR), active toggles, and timers.
- Keep key hints visible at bottom when space allows.

---

## 9) Acceptance Checklist
- Keybindings list matches deliverables (next/prev, size adjust, layout toggle, re-layout, zoom/fit, metrics toggle).
- Layout behavior specified for 80x24, 120x40, 200x60.
- Metrics list includes parse/layout/render times + required diagnostics.
- State model described with enough detail to implement without external docs.
