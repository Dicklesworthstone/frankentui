# Visual FX (Backdrops)

FrankenTUI visual FX are **cell-background backdrops**: deterministic effects that render *behind* normal widgets by writing `PackedRgba` background colors into a caller-owned buffer.

This is intentionally scoped:
- Backdrops do **not** emit glyphs.
- Backdrops must be **tiny-area safe** (0x0 sizes must not panic).
- Backdrops should be **deterministic** given explicit inputs (no hidden globals).
- Backdrops should not require **per-frame allocations** (reuse internal state/caches).

## Feature Flags

All visual FX APIs are opt-in via `ftui-extras` Cargo features:

- `visual-fx`: Core types + Backdrop widget + CPU helpers.
- `visual-fx-metaballs`: Metaballs effect (depends on `visual-fx`).
- `visual-fx-plasma`: Plasma effect (depends on `visual-fx`).
- `fx-gpu`: Optional GPU acceleration (strictly opt-in; no GPU deps unless enabled).

### GPU Runtime Flags

- `FTUI_FX_GPU_DISABLE=1` disables GPU usage even when `fx-gpu` is enabled.
- `FTUI_FX_GPU_FORCE_FAIL=1` forces GPU init failure (test hook) and verifies silent CPU fallback.

## Core API

Core types live in `ftui_extras::visual_fx`:

- `FxQuality`: Quality levels (`Full`, `Reduced`, `Minimal`, `Off`) mapped from render budget.
- `ThemeInputs`: Resolved theme colors needed by FX (data-only boundary).
- `FxContext`: Call-site provided render context (dims/time/quality/theme).
- `BackdropFx`: Trait for background-only effects writing into `&mut [PackedRgba]`.

Row-major layout:

`out[(y * width + x)]` for 0 <= x < width, 0 <= y < height.

See: `crates/ftui-extras/src/visual_fx.rs`.

## Composition Model

The Backdrop widget enables layering animated backgrounds behind any widget without modifying the child widget's rendering logic.

### Basic Composition

```rust
use ftui_extras::visual_fx::{Backdrop, PlasmaFx, PlasmaPalette, ThemeInputs};

// Create the backdrop with an effect
let theme = ThemeInputs::default_dark();
let fx = PlasmaFx::new(PlasmaPalette::Aurora);
let mut backdrop = Backdrop::new(Box::new(fx), theme);

// Option 1: render_with (imperative)
backdrop.render_with(area, frame, &my_widget);

// Option 2: over() composition (returns WithBackdrop)
backdrop.over(&my_widget).render(area, frame);
```

### Builder-Style Configuration

Backdrop supports chained builder methods for ergonomic one-liner setup:

```rust
let backdrop = Backdrop::new(Box::new(fx), theme)
    .with_effect_opacity(0.25)      // How visible the effect is
    .with_scrim(Scrim::vignette(0.3)) // Darkening overlay
    .with_quality_override(Some(FxQuality::Reduced)); // Force quality

backdrop.over(&child).render(area, frame);
```

### Presets

For common use cases, presets provide sensible defaults:

```rust
// Subtle: 15% opacity, no scrim (prioritizes legibility)
Backdrop::new(Box::new(fx), theme).subtle().over(&child);

// Vibrant: 50% opacity, vignette scrim (visual impact)
Backdrop::new(Box::new(fx), theme).vibrant().over(&child);
```

## Legibility Policy

Backdrops must not compromise readability. The rendering pipeline enforces this through layering:

```
final_bg = scrim.over(effect.over(base_fill))
```

### Base Fill

Every backdrop starts with an opaque `base_fill` (derived from `ThemeInputs::bg_surface`). This ensures:
- Deterministic output regardless of prior buffer state
- Consistent contrast baseline for foreground content

### Effect Opacity

The effect layer is alpha-blended over the base fill at configurable opacity:

| Opacity | Use Case |
|---------|----------|
| 0.15 | Subtle background texture (legibility-first) |
| 0.25 | Default balance |
| 0.35 | Moderate visibility |
| 0.50 | Vibrant/hero sections |

Higher opacity values make the effect more prominent but may reduce text contrast.

### Scrim (Overlay)

Scrims add darkening overlays to improve foreground contrast:

```rust
// Uniform darkness across the entire area
Scrim::uniform(0.3)

// Soft vignette (darker at edges)
Scrim::vignette(0.5)

// Vertical fade (top to bottom)
Scrim::vertical_fade(0.0, 0.5)  // (top_opacity, bottom_opacity)
```

**Accessibility note**: For text-heavy content, prefer `subtle()` preset or explicit low opacity + scrim to maintain WCAG contrast ratios.

## Performance Policy

Backdrops automatically adapt to the render budget through quality degradation.

### Quality Levels

```rust
pub enum FxQuality {
    Full,     // Maximum detail
    Reduced,  // Simplified calculations
    Minimal,  // Bare minimum
    Off,      // Effect disabled
}
```

### Degradation Mapping

Quality is derived from `frame.buffer.degradation`:

| DegradationLevel | FxQuality |
|------------------|-----------|
| Full | Full |
| SimpleBorders | Reduced |
| NoStyling | Reduced |
| EssentialOnly | Off |
| Skeleton | Off |
| SkipFrame | Off |

This mapping recognizes that backdrops are decorative, non-essential content.

### Area-Based Clamping

Large render areas automatically clamp quality even at `Full` degradation:

```rust
// Area thresholds (cells)
FX_AREA_THRESHOLD_FULL_TO_REDUCED = 4800   // ~80x60
FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL = 9600 // ~120x80
```

This prevents expensive per-cell computations from blocking the render loop.

### Quality Override

For demos or testing, override automatic quality selection:

```rust
// Force full quality regardless of budget
backdrop.set_quality_override(Some(FxQuality::Full));

// Restore automatic quality
backdrop.set_quality_override(None);
```

## Complete Example: Markdown Over Metaballs

```rust
use ftui_core::geometry::Rect;
use ftui_extras::visual_fx::{
    Backdrop, MetaballsFx, Scrim, ThemeInputs,
};
use ftui_render::frame::Frame;
use ftui_widgets::Widget;

struct MarkdownOverlay {
    backdrop: Backdrop,
    markdown: MyMarkdownWidget,
}

impl MarkdownOverlay {
    pub fn new(theme: ThemeInputs) -> Self {
        let fx = MetaballsFx::new(5, 0.15); // 5 balls, 0.15 speed
        let backdrop = Backdrop::new(Box::new(fx), theme)
            .subtle(); // 15% opacity, no scrim

        Self {
            backdrop,
            markdown: MyMarkdownWidget::new(),
        }
    }

    pub fn tick(&mut self, frame_num: u64, time_secs: f64) {
        self.backdrop.set_time(frame_num, time_secs);
    }
}

impl Widget for MarkdownOverlay {
    fn render(&self, area: Rect, frame: &mut Frame) {
        // Quality is automatically derived from frame.buffer.degradation
        self.backdrop.render_with(area, frame, &self.markdown);
    }
}
```

## Troubleshooting

### Flicker or Tearing

**Symptom**: Visual artifacts during animation.

**Causes**:
1. **Missing double-buffering**: Ensure your terminal backend uses proper buffer swapping.
2. **Slow render**: Quality should auto-degrade, but check if `FxQuality::Off` eliminates the issue.

**Fix**: Verify the presenter is using full buffer writes (not partial updates) for animated content.

### Slow Performance

**Symptom**: Frame drops or stuttering.

**Diagnosis**:
```rust
// Check effective quality
let quality = FxQuality::from_degradation_with_area(
    frame.buffer.degradation,
    area.width as usize * area.height as usize
);
println!("FX quality: {:?}", quality);
```

**Fixes**:
1. Reduce effect opacity (lighter blending).
2. Use `Reduced` or `Minimal` quality presets.
3. For very large areas, consider disabling FX entirely.

### Effect Not Visible

**Symptom**: Background appears solid (no animation).

**Causes**:
1. `FxQuality::Off` (degradation too aggressive)
2. Effect opacity too low
3. Scrim opacity too high (obscuring the effect)

**Fix**: Use quality override for testing:
```rust
backdrop.set_quality_override(Some(FxQuality::Full));
backdrop.set_effect_opacity(0.5);
backdrop.set_scrim(Scrim::Off);
```

## JSONL Telemetry

For performance analysis, FX events can be logged in JSONL format:

```json
{"event":"fx_render","quality":"Reduced","area_cells":4800,"duration_us":1234,"effect":"plasma"}
```

See the telemetry documentation for the full event schema.

## Related Beads

- `bd-l8x9.1.5`: Canvas metaball adapter (sampling API)
- `bd-l8x9.2.3`: Backdrop composition helpers (builder methods)
- `bd-l8x9.2.4`: Backdrop runtime integration (degradation -> FxQuality)
- `bd-l8x9.8.2`: E2E backdrop scenarios
