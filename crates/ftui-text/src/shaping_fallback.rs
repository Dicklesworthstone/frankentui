#![forbid(unsafe_code)]

//! Deterministic fallback path for shaped text rendering.
//!
//! When the shaping engine is unavailable (no font data, feature disabled,
//! or runtime budget exceeded), this module provides a guaranteed fallback
//! that preserves:
//!
//! 1. **Semantic correctness**: all grapheme clusters are rendered.
//! 2. **Interaction stability**: cursor, selection, and copy produce
//!    identical results regardless of whether shaping was used.
//! 3. **Determinism**: the same input always produces the same output.
//!
//! # Fallback strategy
//!
//! The [`ShapingFallback`] struct wraps an optional shaper and transparently
//! degrades when shaping is unavailable or fails:
//!
//! ```text
//!   RustybuzzShaper available → use shaped rendering
//!       ↓ (failure or unavailable)
//!   NoopShaper → terminal/monospace rendering (always succeeds)
//! ```
//!
//! Both paths produce a [`ShapedLineLayout`] with identical interface,
//! ensuring downstream code (cursor navigation, selection, copy) works
//! without branching on which path was taken.
//!
//! # Example
//!
//! ```
//! use ftui_text::shaping_fallback::{ShapingFallback, FallbackEvent};
//! use ftui_text::shaping::NoopShaper;
//! use ftui_text::script_segmentation::{Script, RunDirection};
//!
//! // Create a fallback that always uses NoopShaper (terminal mode).
//! let fallback = ShapingFallback::terminal();
//! let (layout, event) = fallback.shape_line("Hello!", Script::Latin, RunDirection::Ltr);
//!
//! assert_eq!(layout.total_cells(), 6);
//! assert_eq!(event, FallbackEvent::NoopUsed);
//! ```

use crate::layout_policy::{LayoutTier, RuntimeCapability};
use crate::script_segmentation::{RunDirection, Script};
use crate::shaped_render::ShapedLineLayout;
use crate::shaping::{FontFeatures, NoopShaper, ShapedRun, TextShaper};

// ---------------------------------------------------------------------------
// FallbackEvent — what happened during shaping
// ---------------------------------------------------------------------------

/// Diagnostic event describing which path was taken.
///
/// Useful for telemetry, logging, and adaptive quality controllers that
/// may want to track fallback frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackEvent {
    /// Full shaping was used successfully.
    ShapedSuccessfully,
    /// The shaper was invoked but the result was rejected (e.g., empty
    /// output for non-empty input). Fell back to NoopShaper.
    ShapingRejected,
    /// No shaper was available; used NoopShaper directly.
    NoopUsed,
    /// Shaping was skipped because the runtime tier doesn't require it.
    SkippedByPolicy,
}

impl FallbackEvent {
    /// Whether shaping was actually performed.
    #[inline]
    pub const fn was_shaped(&self) -> bool {
        matches!(self, Self::ShapedSuccessfully)
    }

    /// Whether a fallback was triggered.
    #[inline]
    pub const fn is_fallback(&self) -> bool {
        !self.was_shaped()
    }
}

// ---------------------------------------------------------------------------
// FallbackStats — counters for monitoring
// ---------------------------------------------------------------------------

/// Accumulated fallback statistics for monitoring quality degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FallbackStats {
    /// Total lines processed.
    pub total_lines: u64,
    /// Lines that used full shaping.
    pub shaped_lines: u64,
    /// Lines that fell back to NoopShaper.
    pub fallback_lines: u64,
    /// Lines where shaping was rejected after attempt.
    pub rejected_lines: u64,
    /// Lines skipped by policy.
    pub skipped_lines: u64,
}

impl FallbackStats {
    /// Record a fallback event.
    pub fn record(&mut self, event: FallbackEvent) {
        self.total_lines += 1;
        match event {
            FallbackEvent::ShapedSuccessfully => self.shaped_lines += 1,
            FallbackEvent::ShapingRejected => {
                self.fallback_lines += 1;
                self.rejected_lines += 1;
            }
            FallbackEvent::NoopUsed => self.fallback_lines += 1,
            FallbackEvent::SkippedByPolicy => self.skipped_lines += 1,
        }
    }

    /// Fraction of lines that used full shaping (0.0-1.0).
    pub fn shaping_rate(&self) -> f64 {
        if self.total_lines == 0 {
            return 0.0;
        }
        self.shaped_lines as f64 / self.total_lines as f64
    }

    /// Fraction of lines that fell back (0.0-1.0).
    pub fn fallback_rate(&self) -> f64 {
        if self.total_lines == 0 {
            return 0.0;
        }
        self.fallback_lines as f64 / self.total_lines as f64
    }
}

// ---------------------------------------------------------------------------
// ShapingFallback
// ---------------------------------------------------------------------------

/// Transparent shaping with guaranteed fallback.
///
/// Wraps an optional primary shaper and a `NoopShaper` fallback. Always
/// produces a valid [`ShapedLineLayout`] regardless of whether the primary
/// shaper is available or succeeds.
///
/// The output layout has identical API surface for both paths, so
/// downstream code (cursor, selection, copy, rendering) does not need
/// to branch on which shaping path was used.
pub struct ShapingFallback<S: TextShaper = NoopShaper> {
    /// Primary shaper (may be NoopShaper for terminal mode).
    primary: Option<S>,
    /// Font features to apply during shaping.
    features: FontFeatures,
    /// Minimum tier required for shaping to be attempted.
    /// Below this tier, NoopShaper is used directly.
    shaping_tier: LayoutTier,
    /// Current runtime capabilities.
    capabilities: RuntimeCapability,
    /// Whether to validate shaped output and reject suspicious results.
    validate_output: bool,
}

impl ShapingFallback<NoopShaper> {
    /// Create a terminal-mode fallback (always uses NoopShaper).
    #[must_use]
    pub fn terminal() -> Self {
        Self {
            primary: None,
            features: FontFeatures::default(),
            shaping_tier: LayoutTier::Quality,
            capabilities: RuntimeCapability::TERMINAL,
            validate_output: false,
        }
    }
}

impl<S: TextShaper> ShapingFallback<S> {
    /// Create a fallback with a primary shaper.
    #[must_use]
    pub fn with_shaper(shaper: S, capabilities: RuntimeCapability) -> Self {
        Self {
            primary: Some(shaper),
            features: FontFeatures::default(),
            shaping_tier: LayoutTier::Balanced,
            capabilities,
            validate_output: true,
        }
    }

    /// Set the font features used for shaping.
    pub fn set_features(&mut self, features: FontFeatures) {
        self.features = features;
    }

    /// Set the minimum tier for shaping.
    pub fn set_shaping_tier(&mut self, tier: LayoutTier) {
        self.shaping_tier = tier;
    }

    /// Update runtime capabilities (e.g., after font load/unload).
    pub fn set_capabilities(&mut self, caps: RuntimeCapability) {
        self.capabilities = caps;
    }

    /// Enable or disable output validation.
    pub fn set_validate_output(&mut self, validate: bool) {
        self.validate_output = validate;
    }

    /// Shape a line of text with automatic fallback.
    ///
    /// Returns the layout and a diagnostic event describing which path
    /// was taken. The layout is guaranteed to be valid and non-empty
    /// for non-empty input.
    pub fn shape_line(
        &self,
        text: &str,
        script: Script,
        direction: RunDirection,
    ) -> (ShapedLineLayout, FallbackEvent) {
        if text.is_empty() {
            return (ShapedLineLayout::from_text(""), FallbackEvent::NoopUsed);
        }

        // No primary shaper available — use NoopShaper directly.
        let Some(shaper) = &self.primary else {
            return (ShapedLineLayout::from_text(text), FallbackEvent::NoopUsed);
        };

        // Check if the current tier requires shaping.
        let effective_tier = self.capabilities.best_tier();
        if effective_tier < self.shaping_tier {
            return (
                ShapedLineLayout::from_text(text),
                FallbackEvent::SkippedByPolicy,
            );
        }

        // Try shaping with the primary shaper.
        {
            let run = shaper.shape(text, script, direction, &self.features);

            if self.validate_output
                && let Some(rejection) = validate_shaped_run(text, &run)
            {
                tracing::debug!(
                    text_len = text.len(),
                    glyph_count = run.glyphs.len(),
                    reason = %rejection,
                    "Shaped output rejected, falling back to NoopShaper"
                );
                return (
                    ShapedLineLayout::from_text(text),
                    FallbackEvent::ShapingRejected,
                );
            }

            (
                ShapedLineLayout::from_run(text, &run),
                FallbackEvent::ShapedSuccessfully,
            )
        }
    }

    /// Shape multiple lines with fallback, collecting stats.
    ///
    /// Returns layouts and accumulated statistics.
    pub fn shape_lines(
        &self,
        lines: &[&str],
        script: Script,
        direction: RunDirection,
    ) -> (Vec<ShapedLineLayout>, FallbackStats) {
        let mut layouts = Vec::with_capacity(lines.len());
        let mut stats = FallbackStats::default();

        for text in lines {
            let (layout, event) = self.shape_line(text, script, direction);
            stats.record(event);
            layouts.push(layout);
        }

        (layouts, stats)
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a shaped run and return a rejection reason if invalid.
///
/// Checks for common shaping failures:
/// - Empty output for non-empty input
/// - Glyph count dramatically exceeding text length (runaway shaping)
/// - All zero advances (broken font/shaper)
fn validate_shaped_run(text: &str, run: &ShapedRun) -> Option<&'static str> {
    if text.is_empty() {
        return None; // Empty input is always valid
    }

    // Rejection: no glyphs produced for non-empty text.
    if run.glyphs.is_empty() {
        return Some("no glyphs produced for non-empty input");
    }

    // Rejection: glyph count > 4x text byte length (runaway).
    // Legitimate cases (complex scripts, ligature decomposition) rarely
    // exceed 2x. 4x gives ample headroom.
    if run.glyphs.len() > text.len() * 4 {
        return Some("glyph count exceeds 4x text byte length");
    }

    // Rejection: all advances are zero (broken font).
    if run.glyphs.iter().all(|g| g.x_advance == 0) {
        return Some("all glyph advances are zero");
    }

    None
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Terminal mode
    // -----------------------------------------------------------------------

    #[test]
    fn terminal_fallback() {
        let fb = ShapingFallback::terminal();
        let (layout, event) = fb.shape_line("Hello", Script::Latin, RunDirection::Ltr);

        assert_eq!(layout.total_cells(), 5);
        assert_eq!(event, FallbackEvent::NoopUsed);
    }

    #[test]
    fn terminal_empty_input() {
        let fb = ShapingFallback::terminal();
        let (layout, event) = fb.shape_line("", Script::Latin, RunDirection::Ltr);

        assert!(layout.is_empty());
        assert_eq!(event, FallbackEvent::NoopUsed);
    }

    #[test]
    fn terminal_wide_chars() {
        let fb = ShapingFallback::terminal();
        let (layout, _) = fb.shape_line("\u{4E16}\u{754C}", Script::Han, RunDirection::Ltr);

        assert_eq!(layout.total_cells(), 4); // 2 CJK chars × 2 cells each
    }

    // -----------------------------------------------------------------------
    // With shaper
    // -----------------------------------------------------------------------

    #[test]
    fn noop_shaper_primary() {
        let fb = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::TERMINAL);
        let (layout, event) = fb.shape_line("Hello", Script::Latin, RunDirection::Ltr);

        assert_eq!(layout.total_cells(), 5);
        // TERMINAL best_tier is Balanced, shaping_tier is Balanced → tier check passes,
        // NoopShaper shapes successfully.
        assert_eq!(event, FallbackEvent::ShapedSuccessfully);
    }

    #[test]
    fn noop_shaper_with_full_caps() {
        let fb = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::FULL);
        let (layout, event) = fb.shape_line("Hello", Script::Latin, RunDirection::Ltr);

        assert_eq!(layout.total_cells(), 5);
        assert_eq!(event, FallbackEvent::ShapedSuccessfully);
    }

    // -----------------------------------------------------------------------
    // Validation
    // -----------------------------------------------------------------------

    #[test]
    fn validate_empty_run() {
        let run = ShapedRun {
            glyphs: vec![],
            total_advance: 0,
        };
        assert!(validate_shaped_run("Hello", &run).is_some());
    }

    #[test]
    fn validate_empty_input() {
        let run = ShapedRun {
            glyphs: vec![],
            total_advance: 0,
        };
        assert!(validate_shaped_run("", &run).is_none());
    }

    #[test]
    fn validate_zero_advances() {
        use crate::shaping::ShapedGlyph;

        let run = ShapedRun {
            glyphs: vec![
                ShapedGlyph {
                    glyph_id: 1,
                    cluster: 0,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                },
                ShapedGlyph {
                    glyph_id: 2,
                    cluster: 1,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                },
            ],
            total_advance: 0,
        };
        assert!(validate_shaped_run("AB", &run).is_some());
    }

    #[test]
    fn validate_valid_run() {
        use crate::shaping::ShapedGlyph;

        let run = ShapedRun {
            glyphs: vec![
                ShapedGlyph {
                    glyph_id: 1,
                    cluster: 0,
                    x_advance: 1,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                },
                ShapedGlyph {
                    glyph_id: 2,
                    cluster: 1,
                    x_advance: 1,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                },
            ],
            total_advance: 2,
        };
        assert!(validate_shaped_run("AB", &run).is_none());
    }

    // -----------------------------------------------------------------------
    // Fallback stats
    // -----------------------------------------------------------------------

    #[test]
    fn stats_tracking() {
        let mut stats = FallbackStats::default();

        stats.record(FallbackEvent::ShapedSuccessfully);
        stats.record(FallbackEvent::ShapedSuccessfully);
        stats.record(FallbackEvent::NoopUsed);
        stats.record(FallbackEvent::ShapingRejected);

        assert_eq!(stats.total_lines, 4);
        assert_eq!(stats.shaped_lines, 2);
        assert_eq!(stats.fallback_lines, 2);
        assert_eq!(stats.rejected_lines, 1);
        assert_eq!(stats.shaping_rate(), 0.5);
        assert_eq!(stats.fallback_rate(), 0.5);
    }

    #[test]
    fn stats_empty() {
        let stats = FallbackStats::default();
        assert_eq!(stats.shaping_rate(), 0.0);
        assert_eq!(stats.fallback_rate(), 0.0);
    }

    // -----------------------------------------------------------------------
    // Batch shaping
    // -----------------------------------------------------------------------

    #[test]
    fn shape_lines_batch() {
        let fb = ShapingFallback::terminal();
        let lines = vec!["Hello", "World", "\u{4E16}\u{754C}"];

        let (layouts, stats) = fb.shape_lines(&lines, Script::Latin, RunDirection::Ltr);

        assert_eq!(layouts.len(), 3);
        assert_eq!(stats.total_lines, 3);
        assert_eq!(stats.fallback_lines, 3);
    }

    // -----------------------------------------------------------------------
    // FallbackEvent predicates
    // -----------------------------------------------------------------------

    #[test]
    fn event_predicates() {
        assert!(FallbackEvent::ShapedSuccessfully.was_shaped());
        assert!(!FallbackEvent::ShapedSuccessfully.is_fallback());

        assert!(!FallbackEvent::NoopUsed.was_shaped());
        assert!(FallbackEvent::NoopUsed.is_fallback());

        assert!(!FallbackEvent::ShapingRejected.was_shaped());
        assert!(FallbackEvent::ShapingRejected.is_fallback());

        assert!(!FallbackEvent::SkippedByPolicy.was_shaped());
        assert!(FallbackEvent::SkippedByPolicy.is_fallback());
    }

    // -----------------------------------------------------------------------
    // Determinism: both paths produce consistent layouts
    // -----------------------------------------------------------------------

    #[test]
    fn shaped_and_unshaped_same_total_cells() {
        let text = "Hello World!";

        // Shaped path (via NoopShaper → shaped successfully with FULL caps).
        let fb_shaped = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::FULL);
        let (layout_shaped, _) = fb_shaped.shape_line(text, Script::Latin, RunDirection::Ltr);

        // Unshaped path (terminal fallback).
        let fb_unshaped = ShapingFallback::terminal();
        let (layout_unshaped, _) = fb_unshaped.shape_line(text, Script::Latin, RunDirection::Ltr);

        // NoopShaper should produce identical total cell counts.
        assert_eq!(layout_shaped.total_cells(), layout_unshaped.total_cells());
    }

    #[test]
    fn shaped_and_unshaped_identical_interaction() {
        let text = "A\u{4E16}B";

        let fb_shaped = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::FULL);
        let (layout_s, _) = fb_shaped.shape_line(text, Script::Latin, RunDirection::Ltr);

        let fb_unshaped = ShapingFallback::terminal();
        let (layout_u, _) = fb_unshaped.shape_line(text, Script::Latin, RunDirection::Ltr);

        // Cluster maps should agree on byte↔cell mappings.
        let cm_s = layout_s.cluster_map();
        let cm_u = layout_u.cluster_map();

        for byte in [0, 1, 4] {
            assert_eq!(
                cm_s.byte_to_cell(byte),
                cm_u.byte_to_cell(byte),
                "byte_to_cell mismatch at byte {byte}"
            );
        }

        for cell in 0..layout_s.total_cells() {
            assert_eq!(
                cm_s.cell_to_byte(cell),
                cm_u.cell_to_byte(cell),
                "cell_to_byte mismatch at cell {cell}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Configuration
    // -----------------------------------------------------------------------

    #[test]
    fn set_features() {
        let mut fb = ShapingFallback::terminal();
        fb.set_features(FontFeatures::default());
        // Just verifies no panic.
        let (layout, _) = fb.shape_line("test", Script::Latin, RunDirection::Ltr);
        assert_eq!(layout.total_cells(), 4);
    }

    #[test]
    fn set_shaping_tier() {
        let mut fb = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::FULL);
        fb.set_shaping_tier(LayoutTier::Quality);

        // FULL caps support Quality tier, so shaping should still work.
        let (_, event) = fb.shape_line("test", Script::Latin, RunDirection::Ltr);
        assert_eq!(event, FallbackEvent::ShapedSuccessfully);
    }
}
