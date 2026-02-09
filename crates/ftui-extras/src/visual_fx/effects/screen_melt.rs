//! Doom screen melt (wipe) effect.
//!
//! Authentic port of `f_wipe.c` from the Doom GPL source: each column
//! melts down independently, revealing the inner effect underneath.
//! The frozen frame slides down while the new content is revealed.
//!
//! # Algorithm
//!
//! Column offsets start at random negative values. Each tick:
//! - If offset < 0: increment by 1
//! - If offset < height: advance by `min(offset+1, 8)` (accelerating)
//!
//! # Determinism
//!
//! Uses xorshift32 seeded from a fixed seed for reproducible column offsets.
//!
//! # Usage
//!
//! Wraps an inner `BackdropFx` and composites a frozen snapshot over it.
//! The melt effect automatically completes when all columns pass the bottom.

use crate::visual_fx::{BackdropFx, FxContext};
use ftui_render::cell::PackedRgba;

// ---------------------------------------------------------------------------
// Xorshift32 RNG (same as doom_fire)
// ---------------------------------------------------------------------------

#[inline]
fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

// ---------------------------------------------------------------------------
// ScreenMeltFx
// ---------------------------------------------------------------------------

/// Doom screen melt wipe effect.
///
/// Captures a "frozen" frame and reveals the inner effect underneath as
/// columns independently melt downward, mimicking the classic Doom level
/// transition wipe.
///
/// # Quality Degradation
///
/// - `Full`/`Reduced`: Normal column update speed
/// - `Minimal`: Only update every other column per frame
/// - `Off`: No rendering
pub struct ScreenMeltFx {
    /// Per-column y-offsets controlling the melt progress.
    column_offsets: Vec<i32>,
    /// The frozen frame being melted away.
    frozen_frame: Vec<PackedRgba>,
    /// The inner effect revealed underneath.
    inner: Box<dyn BackdropFx>,
    /// Scratch buffer for inner rendering.
    inner_buf: Vec<PackedRgba>,
    /// Whether the melt has been initialized.
    started: bool,
    /// Whether the melt is complete (all columns past bottom).
    done: bool,
    /// RNG state for column offset initialization.
    rng_seed: u32,
    /// Cached width.
    last_width: u16,
    /// Cached height.
    last_height: u16,
}

impl ScreenMeltFx {
    /// Create a new screen melt wrapping an inner effect.
    ///
    /// The melt starts automatically on the first render. The frozen
    /// content is captured from the output buffer's initial state.
    pub fn new(inner: Box<dyn BackdropFx>) -> Self {
        Self::with_seed(inner, 0xDEAD_BEEF)
    }

    /// Create a screen melt with a specific RNG seed for deterministic offsets.
    pub fn with_seed(inner: Box<dyn BackdropFx>, seed: u32) -> Self {
        Self {
            column_offsets: Vec::new(),
            frozen_frame: Vec::new(),
            inner,
            inner_buf: Vec::new(),
            started: false,
            done: false,
            rng_seed: seed,
            last_width: 0,
            last_height: 0,
        }
    }

    /// Reset the melt to start again.
    pub fn reset(&mut self) {
        self.started = false;
        self.done = false;
    }

    /// Returns true if the melt animation is complete.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Access the inner effect.
    pub fn inner(&self) -> &dyn BackdropFx {
        &*self.inner
    }

    /// Access the inner effect mutably.
    pub fn inner_mut(&mut self) -> &mut dyn BackdropFx {
        &mut *self.inner
    }

    /// Initialize column offsets using the Doom algorithm.
    ///
    /// Column 0 gets a random offset in [-15, 0].
    /// Each subsequent column is prev +/- rand()%3 - 1, clamped to [-15, 0].
    fn init_offsets(&mut self, width: u16) {
        let w = width as usize;
        if w > self.column_offsets.len() {
            self.column_offsets.resize(w, 0);
        }

        let mut rng = self.rng_seed | 1;
        let first = -((xorshift32(&mut rng) % 16) as i32);
        self.column_offsets[0] = first;

        for x in 1..w {
            let r = (xorshift32(&mut rng) % 3) as i32 - 1; // -1, 0, or 1
            let prev = self.column_offsets[x - 1];
            self.column_offsets[x] = (prev + r).clamp(-15, 0);
        }
    }

    /// Advance the melt animation one tick.
    fn advance(&mut self) {
        if self.done {
            return;
        }

        let w = self.last_width as usize;
        let h = self.last_height as i32;
        let mut all_done = true;

        for x in 0..w {
            let y = self.column_offsets[x];
            if y < 0 {
                self.column_offsets[x] = y + 1;
                all_done = false;
            } else if y < h {
                // Doom's acceleration: dy = min(y+1, 8)
                let dy = (y + 1).min(8);
                self.column_offsets[x] = y + dy;
                all_done = false;
            }
            // else: column is done (y >= h)
        }

        self.done = all_done;
    }
}

impl BackdropFx for ScreenMeltFx {
    fn name(&self) -> &'static str {
        "Screen Melt"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.inner.resize(width, height);
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        let w = ctx.width as usize;
        let h = ctx.height as usize;
        if w == 0 || h == 0 {
            return;
        }

        let len = w * h;

        // Ensure inner buffer capacity
        if self.inner_buf.len() < len {
            self.inner_buf.resize(len, PackedRgba::rgb(0, 0, 0));
        }

        // Render inner effect
        self.inner.render(ctx, &mut self.inner_buf[..len]);

        // First render: capture the current output buffer as the frozen frame
        if !self.started {
            self.last_width = ctx.width;
            self.last_height = ctx.height;

            // Capture current output as the frozen frame
            if self.frozen_frame.len() < len {
                self.frozen_frame.resize(len, PackedRgba::rgb(0, 0, 0));
            }
            self.frozen_frame[..len].copy_from_slice(&out[..len]);

            self.init_offsets(ctx.width);
            self.started = true;
        }

        // Handle dimension changes
        if self.last_width != ctx.width || self.last_height != ctx.height {
            self.last_width = ctx.width;
            self.last_height = ctx.height;
            // Re-init on resize
            self.init_offsets(ctx.width);
        }

        // If done, just show inner
        if self.done {
            out[..len].copy_from_slice(&self.inner_buf[..len]);
            return;
        }

        // Advance the melt
        self.advance();

        // Composite: row-major iteration for cache-friendly access.
        // For each pixel, check whether it's above or below its column's melt offset.
        let frozen_len = self.frozen_frame.len();
        let h_i32 = h as i32;
        for y in 0..h {
            let row_base = y * w;
            let y_i32 = y as i32;
            for x in 0..w {
                let idx = row_base + x;
                let offset = self.column_offsets[x];
                if y_i32 < offset {
                    // Above the melt line: show the frozen content shifted down
                    let src_y = y_i32 - offset + h_i32;
                    if src_y >= 0 && (src_y as usize) < h {
                        let src_idx = src_y as usize * w + x;
                        if src_idx < frozen_len {
                            out[idx] = self.frozen_frame[src_idx];
                        }
                    }
                } else if offset >= 0 {
                    // Below the melt line: show inner effect
                    out[idx] = self.inner_buf[idx];
                } else {
                    // Offset is negative: show frozen frame (hasn't started melting yet)
                    if idx < frozen_len {
                        out[idx] = self.frozen_frame[idx];
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for ScreenMeltFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenMeltFx")
            .field("started", &self.started)
            .field("done", &self.done)
            .field("last_width", &self.last_width)
            .field("last_height", &self.last_height)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_fx::{FxQuality, ThemeInputs};

    /// Simple solid-color effect for testing.
    struct SolidFx {
        color: PackedRgba,
    }
    impl BackdropFx for SolidFx {
        fn name(&self) -> &'static str {
            "Solid"
        }
        fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
            let len = ctx.width as usize * ctx.height as usize;
            for p in out.iter_mut().take(len) {
                *p = self.color;
            }
        }
    }

    fn default_theme() -> ThemeInputs {
        ThemeInputs::default_dark()
    }

    fn make_ctx(width: u16, height: u16, frame: u64) -> FxContext<'static> {
        let theme = Box::leak(Box::new(default_theme()));
        FxContext {
            width,
            height,
            frame,
            time_seconds: frame as f64 / 60.0,
            quality: FxQuality::Full,
            theme,
        }
    }

    #[test]
    fn melt_starts_and_completes() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let mut buf = vec![PackedRgba::rgb(0, 0, 255); 100]; // "frozen" content is blue

        // First frame captures the frozen content
        let ctx = make_ctx(10, 10, 0);
        melt.render(ctx, &mut buf);
        assert!(!melt.is_done());

        // Run many frames until done
        for frame in 1..200 {
            let ctx = make_ctx(10, 10, frame);
            melt.render(ctx, &mut buf);
            if melt.is_done() {
                break;
            }
        }

        assert!(melt.is_done(), "Melt should complete within 200 frames");

        // After completion, should show inner (red)
        let ctx = make_ctx(10, 10, 200);
        melt.render(ctx, &mut buf);
        assert_eq!(buf[0], PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn melt_zero_dimensions() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let ctx = make_ctx(0, 0, 0);
        let mut buf = vec![];
        melt.render(ctx, &mut buf);
        // Should not panic
    }

    #[test]
    fn melt_deterministic() {
        let inner1 = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let inner2 = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut melt1 = ScreenMeltFx::with_seed(inner1, 42);
        let mut melt2 = ScreenMeltFx::with_seed(inner2, 42);

        let mut buf1 = vec![PackedRgba::rgb(0, 0, 255); 200];
        let mut buf2 = vec![PackedRgba::rgb(0, 0, 255); 200];

        for frame in 0..20 {
            let ctx = make_ctx(20, 10, frame);
            melt1.render(ctx, &mut buf1);
            melt2.render(ctx, &mut buf2);
            assert_eq!(buf1, buf2, "Frame {frame} should be identical");
        }
    }

    #[test]
    fn melt_reset() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let mut buf = vec![PackedRgba::rgb(0, 0, 255); 100];

        // Run to completion
        for frame in 0..200 {
            let ctx = make_ctx(10, 10, frame);
            melt.render(ctx, &mut buf);
        }
        assert!(melt.is_done());

        // Reset
        melt.reset();
        assert!(!melt.is_done());
        assert!(!melt.started);
    }

    // ── Xorshift RNG ───────────────────────────────────────────────

    #[test]
    fn xorshift_nonzero_output() {
        let mut state = 1u32;
        for _ in 0..100 {
            let v = xorshift32(&mut state);
            assert_ne!(v, 0, "xorshift32 should not produce zero");
        }
    }

    #[test]
    fn xorshift_deterministic() {
        let mut s1 = 42u32;
        let mut s2 = 42u32;
        for _ in 0..50 {
            assert_eq!(xorshift32(&mut s1), xorshift32(&mut s2));
        }
    }

    #[test]
    fn xorshift_different_seeds_diverge() {
        let mut s1 = 1u32;
        let mut s2 = 2u32;
        let v1 = xorshift32(&mut s1);
        let v2 = xorshift32(&mut s2);
        assert_ne!(v1, v2, "different seeds should produce different output");
    }

    // ── Column offset initialization ───────────────────────────────

    #[test]
    fn init_offsets_range() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 123);
        melt.column_offsets.resize(80, 0);
        melt.init_offsets(80);

        for (i, &offset) in melt.column_offsets.iter().take(80).enumerate() {
            assert!(
                (-15..=0).contains(&offset),
                "column {i} offset {offset} out of [-15, 0]"
            );
        }
    }

    #[test]
    fn init_offsets_adjacent_differ_by_at_most_one() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 99);
        melt.column_offsets.resize(80, 0);
        melt.init_offsets(80);

        for x in 1..80 {
            let diff = (melt.column_offsets[x] - melt.column_offsets[x - 1]).abs();
            assert!(
                diff <= 1,
                "adjacent columns {}/{} differ by {diff}",
                x - 1,
                x
            );
        }
    }

    #[test]
    fn init_offsets_deterministic_with_same_seed() {
        let inner1 = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let inner2 = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut m1 = ScreenMeltFx::with_seed(inner1, 55);
        let mut m2 = ScreenMeltFx::with_seed(inner2, 55);
        m1.column_offsets.resize(40, 0);
        m2.column_offsets.resize(40, 0);
        m1.init_offsets(40);
        m2.init_offsets(40);
        assert_eq!(
            &m1.column_offsets[..40],
            &m2.column_offsets[..40],
            "same seed should produce same offsets"
        );
    }

    // ── Advance mechanics ──────────────────────────────────────────

    #[test]
    fn advance_negative_offsets_increment_by_one() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 1);
        melt.column_offsets = vec![-10];
        melt.last_width = 1;
        melt.last_height = 20;

        melt.advance();
        assert_eq!(melt.column_offsets[0], -9);
    }

    #[test]
    fn advance_positive_offsets_accelerate() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 1);
        melt.last_width = 1;
        melt.last_height = 100;

        // Test acceleration: dy = min(y+1, 8)
        melt.column_offsets = vec![0];
        melt.advance();
        assert_eq!(melt.column_offsets[0], 1); // dy = min(0+1, 8) = 1

        melt.column_offsets = vec![3];
        melt.advance();
        assert_eq!(melt.column_offsets[0], 7); // dy = min(3+1, 8) = 4

        melt.column_offsets = vec![10];
        melt.advance();
        assert_eq!(melt.column_offsets[0], 18); // dy = min(10+1, 8) = 8
    }

    #[test]
    fn advance_marks_done_when_all_columns_past_bottom() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 1);
        melt.last_width = 3;
        melt.last_height = 10;
        melt.column_offsets = vec![10, 10, 10]; // all >= height

        melt.advance();
        assert!(melt.done);
    }

    #[test]
    fn advance_not_done_if_any_column_active() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 1);
        melt.last_width = 3;
        melt.last_height = 10;
        melt.column_offsets = vec![10, 5, 10]; // middle column still active

        melt.advance();
        assert!(!melt.done);
    }

    #[test]
    fn advance_noop_when_already_done() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::with_seed(inner, 1);
        melt.done = true;
        melt.last_width = 1;
        melt.last_height = 10;
        melt.column_offsets = vec![5];

        melt.advance();
        assert_eq!(melt.column_offsets[0], 5, "should not change when done");
    }

    // ── Rendering ──────────────────────────────────────────────────

    #[test]
    fn first_render_captures_frozen_frame() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let mut buf = vec![PackedRgba::rgb(0, 0, 255); 25]; // blue initial

        let ctx = make_ctx(5, 5, 0);
        melt.render(ctx, &mut buf);

        assert!(melt.started);
        // Frozen frame should contain the blue initial content
        assert_eq!(melt.frozen_frame[0], PackedRgba::rgb(0, 0, 255));
    }

    #[test]
    fn completed_melt_shows_inner_effect() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 255, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let mut buf = vec![PackedRgba::rgb(100, 100, 100); 25];

        // Run to completion
        for frame in 0..300 {
            let ctx = make_ctx(5, 5, frame);
            melt.render(ctx, &mut buf);
            if melt.is_done() {
                break;
            }
        }
        assert!(melt.is_done());

        // Render once more after completion
        let ctx = make_ctx(5, 5, 300);
        melt.render(ctx, &mut buf);

        // All pixels should be inner green
        for (i, &px) in buf.iter().enumerate() {
            assert_eq!(px, PackedRgba::rgb(0, 255, 0), "pixel {i} should be green");
        }
    }

    #[test]
    fn different_seeds_produce_different_patterns() {
        let inner1 = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let inner2 = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut m1 = ScreenMeltFx::with_seed(inner1, 1);
        let mut m2 = ScreenMeltFx::with_seed(inner2, 999);

        let mut buf1 = vec![PackedRgba::rgb(0, 0, 255); 200];
        let mut buf2 = vec![PackedRgba::rgb(0, 0, 255); 200];

        // Render a few frames
        for frame in 0..5 {
            let ctx = make_ctx(20, 10, frame);
            m1.render(ctx, &mut buf1);
            m2.render(ctx, &mut buf2);
        }

        // Should differ (different column offsets from different seeds)
        assert_ne!(
            buf1, buf2,
            "different seeds should produce different patterns"
        );
    }

    #[test]
    fn name_returns_expected() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let melt = ScreenMeltFx::new(inner);
        assert_eq!(melt.name(), "Screen Melt");
    }

    #[test]
    fn debug_format_includes_fields() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let melt = ScreenMeltFx::new(inner);
        let debug = format!("{melt:?}");
        assert!(debug.contains("ScreenMeltFx"));
        assert!(debug.contains("started"));
        assert!(debug.contains("done"));
    }

    #[test]
    fn inner_accessor() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let melt = ScreenMeltFx::new(inner);
        assert_eq!(melt.inner().name(), "Solid");
    }

    #[test]
    fn inner_mut_accessor() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(0, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        assert_eq!(melt.inner_mut().name(), "Solid");
    }

    #[test]
    fn reset_allows_restart() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let mut buf = vec![PackedRgba::rgb(0, 0, 255); 100];

        // Run to completion
        for frame in 0..200 {
            let ctx = make_ctx(10, 10, frame);
            melt.render(ctx, &mut buf);
        }
        assert!(melt.is_done());

        // Reset and run again
        melt.reset();
        let ctx = make_ctx(10, 10, 300);
        melt.render(ctx, &mut buf);
        assert!(melt.started);
        assert!(!melt.is_done(), "should be animating again after reset");
    }

    #[test]
    fn melt_completes_in_bounded_frames() {
        let inner = Box::new(SolidFx {
            color: PackedRgba::rgb(255, 0, 0),
        });
        let mut melt = ScreenMeltFx::new(inner);
        let mut buf = vec![PackedRgba::rgb(0, 0, 0); 8000];

        let mut frames = 0;
        for frame in 0..500 {
            let ctx = make_ctx(80, 100, frame);
            melt.render(ctx, &mut buf);
            frames = frame;
            if melt.is_done() {
                break;
            }
        }
        assert!(
            melt.is_done(),
            "80x100 melt should complete within 500 frames (took {frames})"
        );
    }
}
