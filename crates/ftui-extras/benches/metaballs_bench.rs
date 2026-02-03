//! Benchmarks for MetaballsFx compute cost (bd-l8x9.9.2)
//!
//! Performance budgets:
//! - 80x24 Full:    < 500μs (panic at 2ms)
//! - 120x40 Full:   < 1ms   (panic at 4ms)
//! - 240x80 Full:   < 4ms   (panic at 16ms)
//! - Reduced tier:  ~50-75% of Full cost
//! - Minimal tier:  ~25-50% of Full cost
//!
//! Run with: cargo bench -p ftui-extras --bench metaballs_bench --features visual-fx

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

#[cfg(feature = "visual-fx")]
use ftui_extras::visual_fx::{BackdropFx, FxContext, FxQuality, MetaballsFx, ThemeInputs};
#[cfg(feature = "visual-fx")]
use ftui_render::cell::PackedRgba;

// =============================================================================
// Size configurations from acceptance criteria
// =============================================================================

#[cfg(feature = "visual-fx")]
const SIZES: &[(u16, u16)] = &[
    (80, 24),  // Standard terminal
    (120, 40), // Medium terminal
    (240, 80), // Large terminal (threshold trigger)
];

#[cfg(feature = "visual-fx")]
const QUALITY_LEVELS: &[FxQuality] = &[FxQuality::Full, FxQuality::Reduced, FxQuality::Minimal];

// =============================================================================
// MetaballsFx Render Benchmarks
// =============================================================================

#[cfg(feature = "visual-fx")]
fn bench_metaballs_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("visual_fx/metaballs/render");

    let theme = ThemeInputs::default();

    for &(width, height) in SIZES {
        let cells = width as u64 * height as u64;
        group.throughput(Throughput::Elements(cells));

        for &quality in QUALITY_LEVELS {
            let quality_name = match quality {
                FxQuality::Full => "full",
                FxQuality::Reduced => "reduced",
                FxQuality::Minimal => "minimal",
                FxQuality::Off => "off",
            };

            let id = format!("{width}x{height}/{quality_name}");

            group.bench_with_input(BenchmarkId::new("render", &id), &(), |b, _| {
                let mut fx = MetaballsFx::default_theme();
                fx.resize(width, height);
                let mut out = vec![PackedRgba::TRANSPARENT; (width as usize) * (height as usize)];

                b.iter(|| {
                    let ctx = FxContext {
                        width,
                        height,
                        frame: 0,
                        time_seconds: 1.5,
                        quality,
                        theme: &theme,
                    };
                    fx.render(black_box(ctx), black_box(&mut out));
                    black_box(&out);
                });
            });
        }
    }

    group.finish();
}

// =============================================================================
// MetaballsFx Resize Benchmarks (Cache Allocation)
// =============================================================================

#[cfg(feature = "visual-fx")]
fn bench_metaballs_resize(c: &mut Criterion) {
    let mut group = c.benchmark_group("visual_fx/metaballs/resize");

    for &(width, height) in SIZES {
        let cells = width as u64 * height as u64;
        group.throughput(Throughput::Elements(cells));

        let id = format!("{width}x{height}");

        // Cold resize (fresh instance)
        group.bench_with_input(BenchmarkId::new("cold", &id), &(), |b, _| {
            b.iter(|| {
                let mut fx = MetaballsFx::default_theme();
                fx.resize(black_box(width), black_box(height));
                black_box(&fx);
            });
        });

        // Warm resize (same size - should be near zero cost)
        group.bench_with_input(BenchmarkId::new("warm", &id), &(), |b, _| {
            let mut fx = MetaballsFx::default_theme();
            fx.resize(width, height);

            b.iter(|| {
                fx.resize(black_box(width), black_box(height));
                black_box(&fx);
            });
        });
    }

    group.finish();
}

// =============================================================================
// MetaballsFx Time Progression Benchmarks
// =============================================================================

#[cfg(feature = "visual-fx")]
fn bench_metaballs_time_progression(c: &mut Criterion) {
    let mut group = c.benchmark_group("visual_fx/metaballs/time_progression");

    let theme = ThemeInputs::default();
    let (width, height) = (120, 40);
    let cells = width as u64 * height as u64;
    group.throughput(Throughput::Elements(cells));

    // Benchmark rendering at different time points to verify stable cost
    for frame in [0u64, 60, 300, 1800] {
        let time_seconds = frame as f64 / 60.0;

        group.bench_with_input(
            BenchmarkId::new("frame", frame),
            &time_seconds,
            |b, &time| {
                let mut fx = MetaballsFx::default_theme();
                fx.resize(width, height);
                let mut out = vec![PackedRgba::TRANSPARENT; (width as usize) * (height as usize)];

                b.iter(|| {
                    let ctx = FxContext {
                        width,
                        height,
                        frame,
                        time_seconds: time,
                        quality: FxQuality::Full,
                        theme: &theme,
                    };
                    fx.render(black_box(ctx), black_box(&mut out));
                    black_box(&out);
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
// MetaballsFx Ball Count Scaling
// =============================================================================

#[cfg(feature = "visual-fx")]
fn bench_metaballs_ball_scaling(c: &mut Criterion) {
    use ftui_extras::visual_fx::{Metaball, MetaballsPalette, MetaballsParams};

    let mut group = c.benchmark_group("visual_fx/metaballs/ball_scaling");

    let theme = ThemeInputs::default();
    let (width, height) = (120, 40);
    let cells = width as u64 * height as u64;
    group.throughput(Throughput::Elements(cells));

    for ball_count in [1usize, 4, 7, 12] {
        let balls: Vec<Metaball> = (0..ball_count)
            .map(|i| Metaball {
                x: 0.2 + 0.1 * (i as f64),
                y: 0.3 + 0.05 * (i as f64),
                vx: 0.005 * ((i % 3) as f64 - 1.0),
                vy: 0.006 * ((i % 2) as f64 - 0.5),
                radius: 0.12 + 0.02 * (i as f64 % 4.0),
                hue: (i as f64) / (ball_count as f64),
                phase: i as f64,
            })
            .collect();

        let params = MetaballsParams {
            balls,
            palette: MetaballsPalette::ThemeAccents,
            ..MetaballsParams::default()
        };

        group.bench_with_input(
            BenchmarkId::new("balls", ball_count),
            &params,
            |b, params| {
                let mut fx = MetaballsFx::new(params.clone());
                fx.resize(width, height);
                let mut out = vec![PackedRgba::TRANSPARENT; (width as usize) * (height as usize)];

                b.iter(|| {
                    let ctx = FxContext {
                        width,
                        height,
                        frame: 0,
                        time_seconds: 1.5,
                        quality: FxQuality::Full,
                        theme: &theme,
                    };
                    fx.render(black_box(ctx), black_box(&mut out));
                    black_box(&out);
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
// MetaballsFx Palette Comparison
// =============================================================================

#[cfg(feature = "visual-fx")]
fn bench_metaballs_palettes(c: &mut Criterion) {
    use ftui_extras::visual_fx::{MetaballsPalette, MetaballsParams};

    let mut group = c.benchmark_group("visual_fx/metaballs/palettes");

    let theme = ThemeInputs::default();
    let (width, height) = (120, 40);
    let cells = width as u64 * height as u64;
    group.throughput(Throughput::Elements(cells));

    let palettes = [
        ("theme_accents", MetaballsPalette::ThemeAccents),
        ("aurora", MetaballsPalette::Aurora),
        ("lava", MetaballsPalette::Lava),
        ("ocean", MetaballsPalette::Ocean),
    ];

    for (name, palette) in palettes {
        let params = MetaballsParams {
            palette,
            ..MetaballsParams::default()
        };

        group.bench_with_input(BenchmarkId::new("palette", name), &params, |b, params| {
            let mut fx = MetaballsFx::new(params.clone());
            fx.resize(width, height);
            let mut out = vec![PackedRgba::TRANSPARENT; (width as usize) * (height as usize)];

            b.iter(|| {
                let ctx = FxContext {
                    width,
                    height,
                    frame: 0,
                    time_seconds: 1.5,
                    quality: FxQuality::Full,
                    theme: &theme,
                };
                fx.render(black_box(ctx), black_box(&mut out));
                black_box(&out);
            });
        });
    }

    group.finish();
}

// =============================================================================
// Criterion Groups
// =============================================================================

#[cfg(feature = "visual-fx")]
criterion_group!(
    benches,
    bench_metaballs_render,
    bench_metaballs_resize,
    bench_metaballs_time_progression,
    bench_metaballs_ball_scaling,
    bench_metaballs_palettes,
);

#[cfg(not(feature = "visual-fx"))]
fn bench_placeholder(_c: &mut Criterion) {
    // Placeholder when visual-fx feature is not enabled
}

#[cfg(not(feature = "visual-fx"))]
criterion_group!(benches, bench_placeholder);

criterion_main!(benches);
