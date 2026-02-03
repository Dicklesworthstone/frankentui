# Visual FX (Backdrops)

FrankenTUI visual FX are **cell-background backdrops**: deterministic effects that render *behind* normal widgets by writing `PackedRgba` background colors into a caller-owned buffer.

This is intentionally scoped:
- Backdrops do **not** emit glyphs.
- Backdrops must be **tiny-area safe** (0x0 sizes must not panic).
- Backdrops should be **deterministic** given explicit inputs (no hidden globals).
- Backdrops should not require **per-frame allocations** (reuse internal state/caches).

## Feature Flags

All visual FX APIs are opt-in via `ftui-extras` Cargo features:

- `visual-fx`: Core types + (future) Backdrop widget + CPU helpers.
- `visual-fx-metaballs`: Metaballs effect (depends on `visual-fx`).
- `visual-fx-plasma`: Plasma effect (depends on `visual-fx`).
- `fx-gpu`: Optional GPU acceleration (strictly opt-in; no GPU deps unless enabled).

### GPU Runtime Flags

- `FTUI_FX_GPU_DISABLE=1` disables GPU usage even when `fx-gpu` is enabled.
- `FTUI_FX_GPU_FORCE_FAIL=1` forces GPU init failure (test hook) and verifies silent CPU fallback.

## Core API

Core types live in `ftui_extras::visual_fx`:

- `FxQuality`: A stable quality dial (`Low|Medium|High`).
- `ThemeInputs`: Resolved theme colors needed by FX (data-only boundary).
- `FxContext`: Call-site provided render context (dims/time/quality/theme).
- `BackdropFx`: Trait for background-only effects writing into `&mut [PackedRgba]`.

Row-major layout:

`out[(y * width + x)]` for 0 <= x < width, 0 <= y < height.

See: `crates/ftui-extras/src/visual_fx.rs`.

## Related Work

- Theme conversions (`ThemePalette` / `ResolvedTheme` -> `ThemeInputs`): tracked in `bd-l8x9.1.2`.
- Budget/degradation mapping (`DegradationLevel` -> `FxQuality`): tracked in `bd-l8x9.1.3`.
- Backdrop widget + scrim policies: tracked in `bd-l8x9.2.*`.
