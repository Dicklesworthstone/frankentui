//! Microtypographic justification controls for the Knuth-Plass pipeline.
//!
//! This module provides stretch/shrink/spacing penalty configuration that
//! modulates how the line-break optimizer distributes white space across
//! justified text. It complements [`super::vertical_metrics`] (vertical
//! spacing) with horizontal spacing controls.
//!
//! # Design
//!
//! All widths are in **cell columns** (terminal-native units). The stretch
//! and shrink values define the allowable range of space adjustment per
//! space category. The optimizer penalizes deviations from the natural
//! width according to a cubic badness function (TeX convention).
//!
//! # TeX heritage
//!
//! The glue model follows TeX's concept: each space has a natural width,
//! a stretchability, and a shrinkability. The adjustment ratio `r` is
//! computed as `slack / (total_stretch or total_shrink)`, and badness is
//! `|r|³ × scale`.

use std::fmt;

// =========================================================================
// JustifyMode
// =========================================================================

/// Text alignment mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JustifyMode {
    /// Left-aligned (ragged right). No space stretching.
    #[default]
    Left,
    /// Right-aligned (ragged left).
    Right,
    /// Centered text.
    Center,
    /// Fully justified: spaces are stretched/shrunk to fill the line width.
    /// The last line of each paragraph is left-aligned (TeX default).
    Full,
    /// Distributed justification: like Full, but the last line is also
    /// justified (CJK convention).
    Distributed,
}

impl JustifyMode {
    /// Whether this mode requires space modulation (stretch/shrink).
    #[must_use]
    pub const fn requires_justification(&self) -> bool {
        matches!(self, Self::Full | Self::Distributed)
    }

    /// Whether the last line of a paragraph should be justified.
    #[must_use]
    pub const fn justify_last_line(&self) -> bool {
        matches!(self, Self::Distributed)
    }
}

impl fmt::Display for JustifyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Left => write!(f, "left"),
            Self::Right => write!(f, "right"),
            Self::Center => write!(f, "center"),
            Self::Full => write!(f, "full"),
            Self::Distributed => write!(f, "distributed"),
        }
    }
}

// =========================================================================
// SpaceCategory
// =========================================================================

/// Classification of horizontal space types.
///
/// Different categories have different stretch/shrink tolerances.
/// Inter-sentence space is wider than inter-word (French spacing aside).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpaceCategory {
    /// Normal inter-word space.
    #[default]
    InterWord,
    /// Space after sentence-ending punctuation (wider in traditional
    /// typography, same as inter-word in "French spacing" mode).
    InterSentence,
    /// Inter-character spacing (tracking/letter-spacing). Very small
    /// adjustments; used sparingly for micro-justification.
    InterCharacter,
}

impl fmt::Display for SpaceCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InterWord => write!(f, "inter-word"),
            Self::InterSentence => write!(f, "inter-sentence"),
            Self::InterCharacter => write!(f, "inter-character"),
        }
    }
}

// =========================================================================
// GlueSpec
// =========================================================================

/// TeX-style glue specification: natural width + stretch + shrink.
///
/// All values are in 1/256ths of a cell column (sub-cell units) to allow
/// fine-grained control while remaining integer-only.
///
/// The optimizer computes an adjustment ratio `r`:
/// - `r > 0`: stretch by `r × stretch_subcell`
/// - `r < 0`: shrink by `|r| × shrink_subcell`
/// - `r = 0`: use natural width
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlueSpec {
    /// Natural (ideal) width in sub-cell units (1/256 cell column).
    pub natural_subcell: u32,
    /// Maximum stretchability in sub-cell units.
    pub stretch_subcell: u32,
    /// Maximum shrinkability in sub-cell units.
    pub shrink_subcell: u32,
}

/// Sub-cell units per cell column.
pub const SUBCELL_SCALE: u32 = 256;

impl GlueSpec {
    /// A standard inter-word space: 1 cell, stretchable by 50%, shrinkable by 33%.
    pub const WORD_SPACE: Self = Self {
        natural_subcell: SUBCELL_SCALE,     // 1.0 cell
        stretch_subcell: SUBCELL_SCALE / 2, // 0.5 cell
        shrink_subcell: SUBCELL_SCALE / 3,  // ~0.33 cell
    };

    /// Inter-sentence space: 1.5 cells, more stretchable.
    pub const SENTENCE_SPACE: Self = Self {
        natural_subcell: SUBCELL_SCALE * 3 / 2, // 1.5 cells
        stretch_subcell: SUBCELL_SCALE,         // 1.0 cell
        shrink_subcell: SUBCELL_SCALE / 3,      // ~0.33 cell
    };

    /// French spacing: same as word space (no extra after sentences).
    pub const FRENCH_SPACE: Self = Self::WORD_SPACE;

    /// Zero-width glue (for inter-character micro-adjustments).
    pub const INTER_CHAR: Self = Self {
        natural_subcell: 0,
        stretch_subcell: SUBCELL_SCALE / 16, // 1/16 cell max
        shrink_subcell: SUBCELL_SCALE / 32,  // 1/32 cell max
    };

    /// Rigid glue: no stretch, no shrink.
    #[must_use]
    pub const fn rigid(width_subcell: u32) -> Self {
        Self {
            natural_subcell: width_subcell,
            stretch_subcell: 0,
            shrink_subcell: 0,
        }
    }

    /// Compute the effective width at a given adjustment ratio.
    ///
    /// `ratio` is in 1/256ths: positive = stretch, negative = shrink.
    /// Returns the adjusted width in sub-cell units, clamped to
    /// `[natural - shrink, natural + stretch]`.
    #[must_use]
    pub fn adjusted_width(&self, ratio_fixed: i32) -> u32 {
        if ratio_fixed == 0 {
            return self.natural_subcell;
        }

        if ratio_fixed > 0 {
            // Stretch: natural + stretch * ratio / 256
            let delta = (self.stretch_subcell as u64 * ratio_fixed as u64) / SUBCELL_SCALE as u64;
            self.natural_subcell
                .saturating_add(delta.min(self.stretch_subcell as u64) as u32)
        } else {
            // Shrink: natural - shrink * |ratio| / 256
            let abs_ratio = ratio_fixed.unsigned_abs();
            let delta = (self.shrink_subcell as u64 * abs_ratio as u64) / SUBCELL_SCALE as u64;
            self.natural_subcell
                .saturating_sub(delta.min(self.shrink_subcell as u64) as u32)
        }
    }

    /// Total elasticity (stretch + shrink) in sub-cell units.
    #[must_use]
    pub const fn elasticity(&self) -> u32 {
        self.stretch_subcell.saturating_add(self.shrink_subcell)
    }

    /// Whether this glue is fully rigid (no stretch or shrink).
    #[must_use]
    pub const fn is_rigid(&self) -> bool {
        self.stretch_subcell == 0 && self.shrink_subcell == 0
    }
}

impl Default for GlueSpec {
    fn default() -> Self {
        Self::WORD_SPACE
    }
}

impl fmt::Display for GlueSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nat = self.natural_subcell as f64 / SUBCELL_SCALE as f64;
        let st = self.stretch_subcell as f64 / SUBCELL_SCALE as f64;
        let sh = self.shrink_subcell as f64 / SUBCELL_SCALE as f64;
        write!(f, "{nat:.2} +{st:.2} -{sh:.2}")
    }
}

// =========================================================================
// SpacePenalty
// =========================================================================

/// Penalty modifiers for space adjustment quality.
///
/// Higher values make the optimizer work harder to avoid that adjustment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpacePenalty {
    /// Extra demerit for stretching beyond 75% of max stretch.
    pub excessive_stretch: u64,
    /// Extra demerit for shrinking beyond 75% of max shrink.
    pub excessive_shrink: u64,
    /// Extra demerit for any inter-character spacing adjustment.
    pub tracking_penalty: u64,
}

impl SpacePenalty {
    /// Default penalties: moderate discouragement of extreme adjustments.
    pub const DEFAULT: Self = Self {
        excessive_stretch: 50,
        excessive_shrink: 80,
        tracking_penalty: 200,
    };

    /// Permissive: allow aggressive adjustments with low penalty.
    pub const PERMISSIVE: Self = Self {
        excessive_stretch: 10,
        excessive_shrink: 20,
        tracking_penalty: 50,
    };

    /// Strict: strongly discourage any visible adjustment.
    pub const STRICT: Self = Self {
        excessive_stretch: 200,
        excessive_shrink: 300,
        tracking_penalty: 1000,
    };

    /// Evaluate the penalty for a given adjustment ratio and space category.
    ///
    /// `ratio_fixed` is in 1/256ths (positive = stretch, negative = shrink).
    /// Returns additional demerits beyond the base badness.
    #[must_use]
    pub fn evaluate(&self, ratio_fixed: i32, category: SpaceCategory) -> u64 {
        let mut penalty = 0u64;

        // Threshold: 75% of max (192/256)
        const THRESHOLD: i32 = 192;

        if ratio_fixed > THRESHOLD {
            penalty = penalty.saturating_add(self.excessive_stretch);
        } else if ratio_fixed < -THRESHOLD {
            penalty = penalty.saturating_add(self.excessive_shrink);
        }

        if category == SpaceCategory::InterCharacter && ratio_fixed != 0 {
            penalty = penalty.saturating_add(self.tracking_penalty);
        }

        penalty
    }
}

impl Default for SpacePenalty {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// =========================================================================
// JustificationControl
// =========================================================================

/// Unified justification configuration.
///
/// Combines alignment mode, glue specs per space category, and penalty
/// modifiers into a single configuration object that can be passed to
/// the line-break optimizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JustificationControl {
    /// Text alignment mode.
    pub mode: JustifyMode,
    /// Glue spec for inter-word spaces.
    pub word_space: GlueSpec,
    /// Glue spec for inter-sentence spaces.
    pub sentence_space: GlueSpec,
    /// Glue spec for inter-character adjustments (tracking).
    pub char_space: GlueSpec,
    /// Penalty modifiers for adjustment quality.
    pub penalties: SpacePenalty,
    /// Whether to use French spacing (same space after sentences as words).
    pub french_spacing: bool,
    /// Maximum consecutive hyphens before incurring extra penalty.
    pub max_consecutive_hyphens: u8,
    /// Emergency stretch multiplier (1/256ths): applied when no feasible
    /// break exists. 256 = 1x (no extra), 512 = 2x emergency stretch.
    pub emergency_stretch_factor: u32,
}

impl JustificationControl {
    /// Terminal default: left-aligned, rigid spaces.
    pub const TERMINAL: Self = Self {
        mode: JustifyMode::Left,
        word_space: GlueSpec::rigid(SUBCELL_SCALE),
        sentence_space: GlueSpec::rigid(SUBCELL_SCALE),
        char_space: GlueSpec::rigid(0),
        penalties: SpacePenalty::DEFAULT,
        french_spacing: true,
        max_consecutive_hyphens: 0, // no limit in terminal
        emergency_stretch_factor: SUBCELL_SCALE,
    };

    /// Readable: fully justified with moderate elasticity.
    pub const READABLE: Self = Self {
        mode: JustifyMode::Full,
        word_space: GlueSpec::WORD_SPACE,
        sentence_space: GlueSpec::FRENCH_SPACE, // French spacing
        char_space: GlueSpec::rigid(0),         // No tracking
        penalties: SpacePenalty::DEFAULT,
        french_spacing: true,
        max_consecutive_hyphens: 3,
        emergency_stretch_factor: SUBCELL_SCALE * 3 / 2, // 1.5x
    };

    /// Typographic: full justification with fine-grained controls.
    pub const TYPOGRAPHIC: Self = Self {
        mode: JustifyMode::Full,
        word_space: GlueSpec::WORD_SPACE,
        sentence_space: GlueSpec::SENTENCE_SPACE,
        char_space: GlueSpec::INTER_CHAR,
        penalties: SpacePenalty::STRICT,
        french_spacing: false,
        max_consecutive_hyphens: 2,
        emergency_stretch_factor: SUBCELL_SCALE * 2, // 2x
    };

    /// Look up the glue spec for a given space category.
    #[must_use]
    pub const fn glue_for(&self, category: SpaceCategory) -> GlueSpec {
        match category {
            SpaceCategory::InterWord => self.word_space,
            SpaceCategory::InterSentence => {
                if self.french_spacing {
                    self.word_space
                } else {
                    self.sentence_space
                }
            }
            SpaceCategory::InterCharacter => self.char_space,
        }
    }

    /// Compute total natural width for a sequence of space categories.
    #[must_use]
    pub fn total_natural(&self, spaces: &[SpaceCategory]) -> u32 {
        spaces
            .iter()
            .map(|cat| self.glue_for(*cat).natural_subcell)
            .fold(0u32, u32::saturating_add)
    }

    /// Compute total stretchability for a sequence of space categories.
    #[must_use]
    pub fn total_stretch(&self, spaces: &[SpaceCategory]) -> u32 {
        spaces
            .iter()
            .map(|cat| self.glue_for(*cat).stretch_subcell)
            .fold(0u32, u32::saturating_add)
    }

    /// Compute total shrinkability for a sequence of space categories.
    #[must_use]
    pub fn total_shrink(&self, spaces: &[SpaceCategory]) -> u32 {
        spaces
            .iter()
            .map(|cat| self.glue_for(*cat).shrink_subcell)
            .fold(0u32, u32::saturating_add)
    }

    /// Compute the adjustment ratio for a line.
    ///
    /// `slack_subcell` = desired_width - content_width - natural_space_width
    /// (positive means line is too short, needs stretching).
    ///
    /// Returns ratio in 1/256ths, or `None` if adjustment is impossible
    /// (shrink required exceeds total shrinkability).
    #[must_use]
    pub fn adjustment_ratio(
        &self,
        slack_subcell: i32,
        total_stretch: u32,
        total_shrink: u32,
    ) -> Option<i32> {
        if slack_subcell == 0 {
            return Some(0);
        }

        if slack_subcell > 0 {
            // Need to stretch
            if total_stretch == 0 {
                return None; // Cannot stretch rigid glue
            }
            let ratio = (slack_subcell as i64 * SUBCELL_SCALE as i64) / total_stretch as i64;
            Some(ratio.min(i32::MAX as i64) as i32)
        } else {
            // Need to shrink
            if total_shrink == 0 {
                return None; // Cannot shrink rigid glue
            }
            let ratio = (slack_subcell as i64 * SUBCELL_SCALE as i64) / total_shrink as i64;
            // Shrink ratio must not exceed -256 (100% of shrinkability)
            if ratio < -(SUBCELL_SCALE as i64) {
                None // Over-shrunk
            } else {
                Some(ratio as i32)
            }
        }
    }

    /// Compute badness for a given adjustment ratio.
    ///
    /// Uses the standard TeX cubic formula: `|r/256|³ × 10000`.
    /// Returns `u64::MAX` for infeasible adjustments.
    #[must_use]
    pub fn badness(ratio_fixed: i32) -> u64 {
        const BADNESS_SCALE: u64 = 10_000;

        if ratio_fixed == 0 {
            return 0;
        }

        let abs_r = ratio_fixed.unsigned_abs() as u64;
        // r_frac = abs_r / 256 (the true ratio)
        // badness = r_frac³ × BADNESS_SCALE
        //         = abs_r³ / 256³ × BADNESS_SCALE
        //         = abs_r³ × BADNESS_SCALE / 16_777_216
        let cube = abs_r.saturating_mul(abs_r).saturating_mul(abs_r);
        cube.saturating_mul(BADNESS_SCALE) / (SUBCELL_SCALE as u64).pow(3)
    }

    /// Compute total demerits for a line, combining badness and penalties.
    ///
    /// `ratio_fixed`: adjustment ratio in 1/256ths.
    /// `spaces`: the space categories on this line.
    /// `break_penalty`: penalty from the break point itself.
    #[must_use]
    pub fn line_demerits(
        &self,
        ratio_fixed: i32,
        spaces: &[SpaceCategory],
        break_penalty: i64,
    ) -> u64 {
        let badness = Self::badness(ratio_fixed);
        if badness == u64::MAX {
            return u64::MAX;
        }

        // Base demerits: (line_penalty + badness)² + break_penalty
        let base = badness.saturating_add(10); // line_penalty = 10
        let demerits = base.saturating_mul(base);

        // Add break penalty magnitude
        let bp = break_penalty.unsigned_abs();
        let demerits = demerits.saturating_add(bp.saturating_mul(bp));

        // Add space quality penalties
        let space_penalty: u64 = spaces
            .iter()
            .map(|cat| self.penalties.evaluate(ratio_fixed, *cat))
            .sum();

        demerits.saturating_add(space_penalty)
    }

    /// Validate that the configuration is internally consistent.
    ///
    /// Returns a list of warnings (empty = valid).
    #[must_use]
    pub fn validate(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();

        if self.mode.requires_justification() && self.word_space.is_rigid() {
            warnings.push("justified mode with rigid word space cannot modulate spacing");
        }

        if self.word_space.shrink_subcell > self.word_space.natural_subcell {
            warnings.push("word space shrink exceeds natural width (would go negative)");
        }

        if self.sentence_space.shrink_subcell > self.sentence_space.natural_subcell {
            warnings.push("sentence space shrink exceeds natural width (would go negative)");
        }

        if self.emergency_stretch_factor == 0 {
            warnings.push("emergency stretch factor is zero (no emergency fallback)");
        }

        warnings
    }
}

impl Default for JustificationControl {
    fn default() -> Self {
        Self::TERMINAL
    }
}

impl fmt::Display for JustificationControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mode={} word=[{}] french={}",
            self.mode, self.word_space, self.french_spacing
        )
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── JustifyMode ──────────────────────────────────────────────────

    #[test]
    fn left_does_not_require_justification() {
        assert!(!JustifyMode::Left.requires_justification());
    }

    #[test]
    fn full_requires_justification() {
        assert!(JustifyMode::Full.requires_justification());
    }

    #[test]
    fn distributed_requires_justification() {
        assert!(JustifyMode::Distributed.requires_justification());
    }

    #[test]
    fn full_does_not_justify_last_line() {
        assert!(!JustifyMode::Full.justify_last_line());
    }

    #[test]
    fn distributed_justifies_last_line() {
        assert!(JustifyMode::Distributed.justify_last_line());
    }

    #[test]
    fn default_mode_is_left() {
        assert_eq!(JustifyMode::default(), JustifyMode::Left);
    }

    #[test]
    fn mode_display() {
        assert_eq!(format!("{}", JustifyMode::Full), "full");
        assert_eq!(format!("{}", JustifyMode::Center), "center");
    }

    // ── SpaceCategory ────────────────────────────────────────────────

    #[test]
    fn default_category_is_inter_word() {
        assert_eq!(SpaceCategory::default(), SpaceCategory::InterWord);
    }

    #[test]
    fn category_display() {
        assert_eq!(format!("{}", SpaceCategory::InterWord), "inter-word");
        assert_eq!(
            format!("{}", SpaceCategory::InterSentence),
            "inter-sentence"
        );
        assert_eq!(
            format!("{}", SpaceCategory::InterCharacter),
            "inter-character"
        );
    }

    // ── GlueSpec ─────────────────────────────────────────────────────

    #[test]
    fn word_space_constants() {
        let g = GlueSpec::WORD_SPACE;
        assert_eq!(g.natural_subcell, 256);
        assert_eq!(g.stretch_subcell, 128);
        assert_eq!(g.shrink_subcell, 85); // 256/3 = 85
    }

    #[test]
    fn sentence_space_wider() {
        let sentence = GlueSpec::SENTENCE_SPACE;
        let word = GlueSpec::WORD_SPACE;
        assert!(sentence.natural_subcell > word.natural_subcell);
    }

    #[test]
    fn rigid_has_no_elasticity() {
        let g = GlueSpec::rigid(256);
        assert!(g.is_rigid());
        assert_eq!(g.elasticity(), 0);
    }

    #[test]
    fn word_space_is_not_rigid() {
        assert!(!GlueSpec::WORD_SPACE.is_rigid());
    }

    #[test]
    fn adjusted_width_at_zero_is_natural() {
        let g = GlueSpec::WORD_SPACE;
        assert_eq!(g.adjusted_width(0), g.natural_subcell);
    }

    #[test]
    fn adjusted_width_full_stretch() {
        let g = GlueSpec::WORD_SPACE;
        // ratio = 256 (100% stretch)
        let w = g.adjusted_width(256);
        assert_eq!(w, g.natural_subcell + g.stretch_subcell);
    }

    #[test]
    fn adjusted_width_full_shrink() {
        let g = GlueSpec::WORD_SPACE;
        // ratio = -256 (100% shrink)
        let w = g.adjusted_width(-256);
        assert_eq!(w, g.natural_subcell - g.shrink_subcell);
    }

    #[test]
    fn adjusted_width_partial_stretch() {
        let g = GlueSpec::WORD_SPACE;
        // ratio = 128 (50% stretch)
        let w = g.adjusted_width(128);
        // stretch = 128 * 128 / 256 = 64
        assert_eq!(w, g.natural_subcell + 64);
    }

    #[test]
    fn adjusted_width_clamps_stretch() {
        let g = GlueSpec::WORD_SPACE;
        // ratio = 1024 (way over 100%) — should clamp to max stretch
        let w = g.adjusted_width(1024);
        assert_eq!(w, g.natural_subcell + g.stretch_subcell);
    }

    #[test]
    fn adjusted_width_clamps_shrink() {
        let g = GlueSpec::WORD_SPACE;
        // ratio = -1024 (way over 100%) — should clamp to max shrink
        let w = g.adjusted_width(-1024);
        assert_eq!(w, g.natural_subcell - g.shrink_subcell);
    }

    #[test]
    fn rigid_adjusted_width_ignores_ratio() {
        let g = GlueSpec::rigid(512);
        assert_eq!(g.adjusted_width(256), 512);
        assert_eq!(g.adjusted_width(-256), 512);
    }

    #[test]
    fn elasticity_is_sum() {
        let g = GlueSpec::WORD_SPACE;
        assert_eq!(g.elasticity(), g.stretch_subcell + g.shrink_subcell);
    }

    #[test]
    fn glue_display() {
        let s = format!("{}", GlueSpec::WORD_SPACE);
        assert!(s.contains('+'));
        assert!(s.contains('-'));
    }

    #[test]
    fn default_glue_is_word_space() {
        assert_eq!(GlueSpec::default(), GlueSpec::WORD_SPACE);
    }

    #[test]
    fn french_space_equals_word_space() {
        assert_eq!(GlueSpec::FRENCH_SPACE, GlueSpec::WORD_SPACE);
    }

    // ── SpacePenalty ─────────────────────────────────────────────────

    #[test]
    fn penalty_no_adjustment_is_zero() {
        let p = SpacePenalty::DEFAULT;
        assert_eq!(p.evaluate(0, SpaceCategory::InterWord), 0);
    }

    #[test]
    fn penalty_moderate_stretch_is_zero() {
        let p = SpacePenalty::DEFAULT;
        // 128 is within threshold (192)
        assert_eq!(p.evaluate(128, SpaceCategory::InterWord), 0);
    }

    #[test]
    fn penalty_excessive_stretch() {
        let p = SpacePenalty::DEFAULT;
        // 200 exceeds threshold (192)
        let d = p.evaluate(200, SpaceCategory::InterWord);
        assert_eq!(d, p.excessive_stretch);
    }

    #[test]
    fn penalty_excessive_shrink() {
        let p = SpacePenalty::DEFAULT;
        let d = p.evaluate(-200, SpaceCategory::InterWord);
        assert_eq!(d, p.excessive_shrink);
    }

    #[test]
    fn penalty_tracking_always_penalized() {
        let p = SpacePenalty::DEFAULT;
        let d = p.evaluate(1, SpaceCategory::InterCharacter);
        assert_eq!(d, p.tracking_penalty);
    }

    #[test]
    fn penalty_tracking_plus_excessive() {
        let p = SpacePenalty::DEFAULT;
        let d = p.evaluate(200, SpaceCategory::InterCharacter);
        assert_eq!(d, p.excessive_stretch + p.tracking_penalty);
    }

    #[test]
    fn penalty_zero_tracking_no_penalty() {
        let p = SpacePenalty::DEFAULT;
        assert_eq!(p.evaluate(0, SpaceCategory::InterCharacter), 0);
    }

    // ── JustificationControl ────────────────────────────────────────

    #[test]
    fn terminal_is_left_rigid() {
        let j = JustificationControl::TERMINAL;
        assert_eq!(j.mode, JustifyMode::Left);
        assert!(j.word_space.is_rigid());
    }

    #[test]
    fn readable_is_full_elastic() {
        let j = JustificationControl::READABLE;
        assert_eq!(j.mode, JustifyMode::Full);
        assert!(!j.word_space.is_rigid());
    }

    #[test]
    fn typographic_has_tracking() {
        let j = JustificationControl::TYPOGRAPHIC;
        assert!(!j.char_space.is_rigid());
    }

    #[test]
    fn french_spacing_overrides_sentence() {
        let j = JustificationControl::READABLE;
        assert!(j.french_spacing);
        assert_eq!(
            j.glue_for(SpaceCategory::InterSentence),
            j.glue_for(SpaceCategory::InterWord)
        );
    }

    #[test]
    fn non_french_uses_sentence_space() {
        let j = JustificationControl::TYPOGRAPHIC;
        assert!(!j.french_spacing);
        assert_ne!(
            j.glue_for(SpaceCategory::InterSentence).natural_subcell,
            j.glue_for(SpaceCategory::InterWord).natural_subcell
        );
    }

    #[test]
    fn total_natural_sums() {
        let j = JustificationControl::READABLE;
        let spaces = vec![SpaceCategory::InterWord; 5];
        assert_eq!(j.total_natural(&spaces), 5 * j.word_space.natural_subcell);
    }

    #[test]
    fn total_stretch_sums() {
        let j = JustificationControl::READABLE;
        let spaces = vec![SpaceCategory::InterWord; 3];
        assert_eq!(j.total_stretch(&spaces), 3 * j.word_space.stretch_subcell);
    }

    #[test]
    fn total_shrink_sums() {
        let j = JustificationControl::READABLE;
        let spaces = vec![SpaceCategory::InterWord; 4];
        assert_eq!(j.total_shrink(&spaces), 4 * j.word_space.shrink_subcell);
    }

    // ── Adjustment ratio ─────────────────────────────────────────────

    #[test]
    fn ratio_zero_slack() {
        let j = JustificationControl::READABLE;
        assert_eq!(j.adjustment_ratio(0, 100, 100), Some(0));
    }

    #[test]
    fn ratio_positive_stretch() {
        let j = JustificationControl::READABLE;
        // slack = 128, stretch = 256 → ratio = 128 * 256 / 256 = 128
        assert_eq!(j.adjustment_ratio(128, 256, 100), Some(128));
    }

    #[test]
    fn ratio_negative_shrink() {
        let j = JustificationControl::READABLE;
        // slack = -64, shrink = 128 → ratio = -64 * 256 / 128 = -128
        assert_eq!(j.adjustment_ratio(-64, 100, 128), Some(-128));
    }

    #[test]
    fn ratio_no_stretch_returns_none() {
        let j = JustificationControl::READABLE;
        assert_eq!(j.adjustment_ratio(100, 0, 100), None);
    }

    #[test]
    fn ratio_no_shrink_returns_none() {
        let j = JustificationControl::READABLE;
        assert_eq!(j.adjustment_ratio(-100, 100, 0), None);
    }

    #[test]
    fn ratio_over_shrink_returns_none() {
        let j = JustificationControl::READABLE;
        // slack = -300, shrink = 100 → ratio = -300 * 256 / 100 = -768 < -256
        assert_eq!(j.adjustment_ratio(-300, 100, 100), None);
    }

    // ── Badness ──────────────────────────────────────────────────────

    #[test]
    fn badness_zero_ratio() {
        assert_eq!(JustificationControl::badness(0), 0);
    }

    #[test]
    fn badness_ratio_256_is_scale() {
        // |256/256|³ × 10000 = 10000
        assert_eq!(JustificationControl::badness(256), 10_000);
    }

    #[test]
    fn badness_negative_same_as_positive() {
        assert_eq!(
            JustificationControl::badness(128),
            JustificationControl::badness(-128)
        );
    }

    #[test]
    fn badness_half_ratio() {
        // |128/256|³ × 10000 = 0.125 × 10000 = 1250
        assert_eq!(JustificationControl::badness(128), 1250);
    }

    #[test]
    fn badness_monotonically_increasing() {
        let b0 = JustificationControl::badness(0);
        let b1 = JustificationControl::badness(64);
        let b2 = JustificationControl::badness(128);
        let b3 = JustificationControl::badness(256);
        assert!(b0 < b1);
        assert!(b1 < b2);
        assert!(b2 < b3);
    }

    // ── Demerits ─────────────────────────────────────────────────────

    #[test]
    fn demerits_zero_ratio_minimal() {
        let j = JustificationControl::READABLE;
        let spaces = vec![SpaceCategory::InterWord; 3];
        let d = j.line_demerits(0, &spaces, 0);
        // badness = 0, base = 10, demerits = 100
        assert_eq!(d, 100);
    }

    #[test]
    fn demerits_increase_with_ratio() {
        let j = JustificationControl::READABLE;
        let spaces = vec![SpaceCategory::InterWord; 3];
        let d1 = j.line_demerits(64, &spaces, 0);
        let d2 = j.line_demerits(128, &spaces, 0);
        assert!(d2 > d1);
    }

    #[test]
    fn demerits_include_break_penalty() {
        let j = JustificationControl::READABLE;
        let spaces = vec![SpaceCategory::InterWord; 3];
        let d0 = j.line_demerits(0, &spaces, 0);
        let d1 = j.line_demerits(0, &spaces, 50);
        assert!(d1 > d0);
    }

    // ── Validation ───────────────────────────────────────────────────

    #[test]
    fn terminal_validates_clean() {
        // Terminal is left-aligned, so rigid word space is fine
        assert!(JustificationControl::TERMINAL.validate().is_empty());
    }

    #[test]
    fn readable_validates_clean() {
        assert!(JustificationControl::READABLE.validate().is_empty());
    }

    #[test]
    fn typographic_validates_clean() {
        assert!(JustificationControl::TYPOGRAPHIC.validate().is_empty());
    }

    #[test]
    fn full_mode_rigid_warns() {
        let mut j = JustificationControl::TERMINAL;
        j.mode = JustifyMode::Full;
        let warnings = j.validate();
        assert!(!warnings.is_empty());
    }

    #[test]
    fn shrink_exceeds_natural_warns() {
        let mut j = JustificationControl::READABLE;
        j.word_space.shrink_subcell = j.word_space.natural_subcell + 1;
        let warnings = j.validate();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("shrink exceeds natural"))
        );
    }

    #[test]
    fn zero_emergency_factor_warns() {
        let mut j = JustificationControl::READABLE;
        j.emergency_stretch_factor = 0;
        let warnings = j.validate();
        assert!(warnings.iter().any(|w| w.contains("emergency")));
    }

    // ── Display ──────────────────────────────────────────────────────

    #[test]
    fn control_display() {
        let s = format!("{}", JustificationControl::READABLE);
        assert!(s.contains("full"));
        assert!(s.contains("french=true"));
    }

    #[test]
    fn default_control_is_terminal() {
        assert_eq!(
            JustificationControl::default(),
            JustificationControl::TERMINAL
        );
    }

    // ── Determinism ──────────────────────────────────────────────────

    #[test]
    fn same_inputs_same_badness() {
        assert_eq!(
            JustificationControl::badness(200),
            JustificationControl::badness(200)
        );
    }

    #[test]
    fn same_inputs_same_demerits() {
        let j = JustificationControl::TYPOGRAPHIC;
        let spaces = vec![SpaceCategory::InterWord; 5];
        let d1 = j.line_demerits(150, &spaces, 50);
        let d2 = j.line_demerits(150, &spaces, 50);
        assert_eq!(d1, d2);
    }

    #[test]
    fn same_inputs_same_ratio() {
        let j = JustificationControl::READABLE;
        assert_eq!(
            j.adjustment_ratio(100, 200, 100),
            j.adjustment_ratio(100, 200, 100)
        );
    }
}
