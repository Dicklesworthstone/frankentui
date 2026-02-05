# Table Theme Specification

## Overview
The TableTheme system unifies styling for **all** table render paths (widget tables + markdown tables). It is deterministic, lightweight, and supports optional, phase-driven effects without internal clocks.

Goals:
- Single, shared API for tables across `ftui-widgets` and markdown rendering.
- Deterministic rendering with explicit phase input (no implicit time).
- Presets that look great at high density and remain a11y-friendly.
- No compatibility shims: the new API is the one true path.

Non-Goals:
- No automatic texture/glyph changes (style only).
- No nondeterministic animation (phase is explicit input).

## Core Data Model

```rust
#[derive(Clone, Debug)]
pub struct TableTheme {
    pub border: Style,
    pub header: Style,
    pub row: Style,
    pub row_alt: Style,
    pub row_selected: Style,
    pub row_hover: Style,
    pub divider: Style,
    pub padding: u8,
    pub column_gap: u8,
    pub row_height: u8,
    pub effects: Vec<TableEffectRule>,
    pub preset_id: Option<TablePresetId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableSection {
    Header,
    Body,
    Footer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableEffectTarget {
    Section(TableSection),
    Row(usize),
    RowRange { start: usize, end: usize },
    Column(usize),
    ColumnRange { start: usize, end: usize },
    AllRows,     // Body rows only
    AllCells,    // Header + body
}

#[derive(Clone, Debug)]
pub enum TableEffect {
    Pulse {
        fg_a: PackedRgba,
        fg_b: PackedRgba,
        bg_a: PackedRgba,
        bg_b: PackedRgba,
        speed: f32,
        phase_offset: f32,
    },
    BreathingGlow {
        fg: PackedRgba,
        bg: PackedRgba,
        intensity: f32,
        speed: f32,
        phase_offset: f32,
        asymmetry: f32,
    },
    GradientSweep {
        gradient: Gradient,
        speed: f32,
        phase_offset: f32,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum BlendMode {
    Replace,
    Additive,
    Multiply,
    Screen,
}

#[derive(Clone, Copy, Debug)]
pub struct StyleMask {
    pub fg: bool,
    pub bg: bool,
    pub attrs: bool,
}

#[derive(Clone, Debug)]
pub struct TableEffectRule {
    pub target: TableEffectTarget,
    pub effect: TableEffect,
    pub priority: u8,
    pub blend_mode: BlendMode,
    pub style_mask: StyleMask,
}
```

Notes:
- `Style` should accept either `ColorToken` or `PackedRgba` (for theme-driven vs concrete colors).
- `TableTheme::resolve_style(ctx, phase)` must be **pure** and **allocation-free**.

## Phase Semantics (Deterministic Animation)
- `phase: f32` is normalized in `[0, 1)`.
- Values outside `[0, 1)` are wrapped using `phase.fract()`.
- Effect phase = `phase + phase_offset`; apply `fract()` again after offset.
- No hidden clocks. The caller supplies `phase` explicitly (e.g., from runtime tick).

## Index Semantics
- Row/column indices are **0-based**.
- Indices refer to **body rows/columns only** (header is excluded).
- Header effects must target `Section(Header)` explicitly.
- `AllRows` targets **body rows only**.
- `AllCells` includes header + body.

## Precedence and Merge Order
Order of application is strict and deterministic:
1. Base style from theme:
   - Header row uses `header`.
   - Body rows use `row` or `row_alt`.
2. State overlays:
   - `row_selected` then `row_hover` (hover can override selected if both true).
3. Explicit row or per-cell styles (from widget/markdown):
   - These override the theme base/state layers.
4. Effects (sorted by `priority`, then stable insertion order):
   - Apply using `style_mask` to avoid clobbering unrelated channels.

This guarantees explicit cell styling is never overwritten by the theme unless the caller chooses to merge earlier.

## Integration Points

### Widget Tables
- `TableTheme.border` and `divider` map to the `Block` border styles.
- Table title styling remains independent (theme should not override `Block` title styles).

### Markdown Tables
- Markdown rendering uses the same `TableTheme` base/states/effects pipeline.
- Markdown-specific padding/column gaps should default to theme values but remain overrideable.

### Degradation Behavior
- If `Frame.degradation.apply_styling == false`, the theme must render as minimal styling (no effects, base colors only).
- Effects are skipped entirely under degradation.

## Presets
Presets are declarative `TableTheme` constructors:

- **aurora**: luminous header, cool zebra rows, crisp borders.
- **graphite**: monochrome, maximum legibility at dense data.
- **neon**: accent header + subtle color sweep for emphasis.

Preset requirements:
- A11y-friendly contrast.
- Deterministic, tasteful effects (no flashing).
- No reliance on terminal truecolor; degrade gracefully to nearest palette.

## JSON Export/Import (TableThemeSpec)

The export/import format is `TableThemeSpec` (pure data, no rendering logic).
It is **strict** (`deny_unknown_fields`) and versioned for forward compatibility.

Key fields:
- `version` (u8): schema version. Current: `1`.
- `name` (optional string): human-friendly label (max 64 chars).
- `preset_id` (optional enum): original preset identifier, if derived.
- `padding`, `column_gap`, `row_height` (u8): layout parameters.
- `styles`: `border`, `header`, `row`, `row_alt`, `row_selected`, `row_hover`, `divider`.
- `effects`: array of effect rules with target, effect, priority, blend_mode, style_mask.

Validation constraints (current):
- `effects` length ≤ 64.
- `styles.*.attrs` length ≤ 16.
- `gradient.stops` length in `[1, 16]`.
- `padding`, `column_gap` in `[0, 8]`.
- `row_height` in `[1, 8]`.
- `speed` ≤ 10.0, `phase_offset` ≤ 1.0, `intensity` ≤ 1.0, `asymmetry` ≤ 0.9.

Minimal example (abridged):

```json
{
  "version": 1,
  "name": "my-dense-preset",
  "preset_id": null,
  "padding": 1,
  "column_gap": 1,
  "row_height": 1,
  "styles": {
    "border": {"fg":{"r":120,"g":130,"b":140,"a":255},"bg":null,"underline":null,"attrs":["Bold"]},
    "header": {"fg":{"r":235,"g":240,"b":255,"a":255},"bg":null,"underline":null,"attrs":["Bold"]},
    "row": {"fg":null,"bg":null,"underline":null,"attrs":[]},
    "row_alt": {"fg":null,"bg":{"r":24,"g":28,"b":36,"a":255},"underline":null,"attrs":[]},
    "row_selected": {"fg":null,"bg":null,"underline":null,"attrs":[]},
    "row_hover": {"fg":null,"bg":null,"underline":null,"attrs":[]},
    "divider": {"fg":{"r":70,"g":80,"b":95,"a":255},"bg":null,"underline":null,"attrs":[]}
  },
  "effects": []
}
```

Export/import flow:
- Export: `TableThemeSpec::from_theme(&theme)` → JSON.
- Import: parse JSON → `TableThemeSpec::validate()` → `into_theme()`.

## Cookbook: Practical Overrides

### 1) Override Header + Zebra Colors
```rust
use ftui_style::{Style, TableTheme};
use ftui_render::cell::PackedRgba;

let theme = TableTheme::terminal_classic()
    .with_header(Style::new().fg(PackedRgba::rgb(235, 240, 255)).bold())
    .with_row_alt(Style::new().bg(PackedRgba::rgb(24, 28, 36)))
    .with_divider(Style::new().fg(PackedRgba::rgb(70, 80, 95)));
```

### 2) Subtle Breathing Highlight for a Single Row
```rust
use ftui_style::{TableEffect, TableEffectRule, TableEffectTarget, TableTheme};
use ftui_render::cell::PackedRgba;

let theme = TableTheme::aurora().with_effect(TableEffectRule::new(
    TableEffectTarget::Row(2),
    TableEffect::BreathingGlow {
        fg: PackedRgba::rgb(235, 245, 255),
        bg: PackedRgba::rgb(30, 40, 58),
        intensity: 0.35,
        speed: 0.6,
        phase_offset: 0.0,
        asymmetry: 0.15,
    },
));
// Supply an explicit phase at render time (deterministic):
// table.theme(theme).theme_phase(0.25);
```

For markdown tables, apply the same theme through `MarkdownTheme` and pass an
explicit phase to the renderer:

```rust
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
use ftui_style::TableTheme;

let theme = TableTheme::aurora();
let md_theme = MarkdownTheme {
    table_theme: theme,
    ..MarkdownTheme::default()
};
let renderer = MarkdownRenderer::new(md_theme).table_effect_phase(0.25);
```

### 3) Preset Selection Guidance
```rust
use ftui_style::{ColorProfile, TableTheme};

let theme = TableTheme::terminal_classic_for(ColorProfile::Ansi16);
```
- `TableTheme::terminal_classic_for(ColorProfile::Ansi16)` for ANSI-only terminals.
- `TableTheme::terminal_classic_for(ColorProfile::Ansi256)` when you want ANSI-safe colors but a bit more range.
- `TableTheme::graphite()` for dense data and maximum legibility.
- `TableTheme::midnight()` for dark terminals; `TableTheme::paper()` for light themes.
- `TableTheme::aurora()` or `TableTheme::neon()` when you want visual emphasis.

## Performance Constraints
- `resolve_style` must be O(number_of_effect_rules) with **no allocations**.
- No string operations in hot paths.
- Preset creation should be cheap and share static palettes where possible.

## Test Plan

### Unit Tests
- `table_theme_phase_wraps`: phase normalization and offset wrapping.
- `table_theme_precedence`: base → state → explicit → effects order.
- `table_theme_targets`: AllRows/AllCells/RowRange semantics.

### Snapshot Tests
- Markdown tables with each preset at 80x24 and 120x40.
- Widget tables with selection/hover rows and effect overlays.

### E2E (PTY)
- Render a table in both markdown + widget modes with the same theme.
- Log: `preset_id`, `phase`, `row_idx`, `style_hash` to verify determinism.

## Migration Notes
- Remove any legacy MarkdownTheme-specific table styling paths.
- Redirect all table styling to `TableTheme` with no compatibility shims.
