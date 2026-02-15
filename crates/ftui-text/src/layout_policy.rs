//! User-facing layout policy presets and deterministic fallback contract.
//!
//! This module ties together the three layout subsystems:
//! - [`super::wrap::ParagraphObjective`] — Knuth-Plass line-break tuning
//! - [`super::vertical_metrics::VerticalPolicy`] — leading, baseline grid, paragraph spacing
//! - [`super::justification::JustificationControl`] — stretch/shrink/spacing modulation
//!
//! It provides three named quality tiers (`Quality`, `Balanced`, `Fast`) and a
//! deterministic fallback contract that maps runtime capabilities to the
//! highest achievable tier.
//!
//! # Fallback semantics
//!
//! Given a [`RuntimeCapability`] descriptor, [`LayoutPolicy::resolve`] returns
//! a fully-resolved [`ResolvedPolicy`] that may have been degraded from the
//! requested tier. The degradation path is:
//!
//! ```text
//!   Quality → Balanced → Fast
//! ```
//!
//! Each step disables features that require capabilities not available at
//! runtime (e.g., proportional fonts, sub-pixel rendering, hyphenation dict).

use std::fmt;

use crate::justification::{JustificationControl, JustifyMode};
use crate::vertical_metrics::{VerticalMetrics, VerticalPolicy};
use crate::wrap::ParagraphObjective;

// =========================================================================
// LayoutTier
// =========================================================================

/// Named quality tiers for text layout.
///
/// Higher tiers produce better output but require more computation and
/// richer runtime capabilities (proportional fonts, sub-pixel positioning).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum LayoutTier {
    /// Minimal: greedy wrapping, no justification, no leading.
    /// Suitable for raw terminal output where every cell counts.
    Fast = 0,
    /// Moderate: optimal line-breaking, French spacing, moderate leading.
    /// Good default for terminal UIs with readable text.
    #[default]
    Balanced = 1,
    /// Full: TeX-class typography with baseline grid, microtypographic
    /// justification, hyphenation, and fine-grained spacing.
    Quality = 2,
}

impl LayoutTier {
    /// The tier one step below, or `None` if already at `Fast`.
    #[must_use]
    pub const fn degrade(&self) -> Option<Self> {
        match self {
            Self::Quality => Some(Self::Balanced),
            Self::Balanced => Some(Self::Fast),
            Self::Fast => None,
        }
    }

    /// All tiers from this one down to Fast (inclusive), in degradation order.
    #[must_use]
    pub fn degradation_chain(&self) -> Vec<Self> {
        let mut chain = vec![*self];
        let mut current = *self;
        while let Some(next) = current.degrade() {
            chain.push(next);
            current = next;
        }
        chain
    }
}

impl fmt::Display for LayoutTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fast => write!(f, "fast"),
            Self::Balanced => write!(f, "balanced"),
            Self::Quality => write!(f, "quality"),
        }
    }
}

// =========================================================================
// RuntimeCapability
// =========================================================================

/// Descriptor of available runtime capabilities.
///
/// The fallback logic inspects these flags to determine which features
/// can be activated at a given tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RuntimeCapability {
    /// Whether proportional (variable-width) fonts are available.
    /// If false, all justification stretch/shrink is meaningless.
    pub proportional_fonts: bool,

    /// Whether sub-pixel positioning is available (e.g., WebGPU renderer).
    /// If false, baseline grid snapping and sub-cell glue are inert.
    pub subpixel_positioning: bool,

    /// Whether a hyphenation dictionary is loaded.
    /// If false, hyphenation break points are not generated.
    pub hyphenation_available: bool,

    /// Whether inter-character spacing (tracking) is supported by the renderer.
    pub tracking_support: bool,

    /// Maximum paragraph length (in words) that can be processed by the
    /// optimal breaker within the frame budget. 0 = unlimited.
    pub max_paragraph_words: usize,
}

impl RuntimeCapability {
    /// Full capabilities: everything available.
    pub const FULL: Self = Self {
        proportional_fonts: true,
        subpixel_positioning: true,
        hyphenation_available: true,
        tracking_support: true,
        max_paragraph_words: 0,
    };

    /// Terminal-only: monospace, no sub-pixel, no hyphenation.
    pub const TERMINAL: Self = Self {
        proportional_fonts: false,
        subpixel_positioning: false,
        hyphenation_available: false,
        tracking_support: false,
        max_paragraph_words: 0,
    };

    /// Web renderer: proportional fonts but potentially limited tracking.
    pub const WEB: Self = Self {
        proportional_fonts: true,
        subpixel_positioning: true,
        hyphenation_available: true,
        tracking_support: false,
        max_paragraph_words: 0,
    };

    /// Check if the given tier's features are supportable.
    #[must_use]
    pub fn supports_tier(&self, tier: LayoutTier) -> bool {
        match tier {
            LayoutTier::Fast => true,     // Always supportable
            LayoutTier::Balanced => true, // Works in monospace too (just less impactful)
            LayoutTier::Quality => {
                // Quality requires proportional fonts for meaningful justification
                self.proportional_fonts
            }
        }
    }

    /// Find the highest tier this capability set can support.
    #[must_use]
    pub fn best_tier(&self) -> LayoutTier {
        if self.supports_tier(LayoutTier::Quality) {
            LayoutTier::Quality
        } else if self.supports_tier(LayoutTier::Balanced) {
            LayoutTier::Balanced
        } else {
            LayoutTier::Fast
        }
    }
}

impl fmt::Display for RuntimeCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "proportional={} subpixel={} hyphen={} tracking={}",
            self.proportional_fonts,
            self.subpixel_positioning,
            self.hyphenation_available,
            self.tracking_support
        )
    }
}

// =========================================================================
// LayoutPolicy
// =========================================================================

/// User-facing layout policy configuration.
///
/// Combines a desired tier with optional overrides. Call [`resolve`] with
/// a [`RuntimeCapability`] to get a fully-resolved configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutPolicy {
    /// Desired quality tier.
    pub tier: LayoutTier,
    /// If true, allow automatic degradation when capabilities are
    /// insufficient. If false, resolution fails with an error.
    pub allow_degradation: bool,
    /// Override the justify mode (ignoring the tier's default).
    pub justify_override: Option<JustifyMode>,
    /// Override the vertical policy (ignoring the tier's default).
    pub vertical_override: Option<VerticalPolicy>,
    /// Line height in sub-pixel units (1/256 px) for vertical metrics.
    /// 0 = use default (16px = 4096 subpx).
    pub line_height_subpx: u32,
}

/// Default line height: 16px in sub-pixel units.
const DEFAULT_LINE_HEIGHT_SUBPX: u32 = 16 * 256;

impl LayoutPolicy {
    /// Quick preset: terminal-optimized.
    pub const FAST: Self = Self {
        tier: LayoutTier::Fast,
        allow_degradation: true,
        justify_override: None,
        vertical_override: None,
        line_height_subpx: 0,
    };

    /// Balanced preset: good for general use.
    pub const BALANCED: Self = Self {
        tier: LayoutTier::Balanced,
        allow_degradation: true,
        justify_override: None,
        vertical_override: None,
        line_height_subpx: 0,
    };

    /// Quality preset: best output.
    pub const QUALITY: Self = Self {
        tier: LayoutTier::Quality,
        allow_degradation: true,
        justify_override: None,
        vertical_override: None,
        line_height_subpx: 0,
    };

    /// The effective line height (using default if unset).
    #[must_use]
    pub const fn effective_line_height(&self) -> u32 {
        if self.line_height_subpx == 0 {
            DEFAULT_LINE_HEIGHT_SUBPX
        } else {
            self.line_height_subpx
        }
    }

    /// Resolve this policy against runtime capabilities.
    ///
    /// Returns a fully-resolved configuration, potentially degraded to
    /// a lower tier if capabilities are insufficient.
    ///
    /// # Errors
    ///
    /// Returns `PolicyError::CapabilityInsufficient` if `allow_degradation`
    /// is false and the requested tier cannot be supported.
    pub fn resolve(&self, caps: &RuntimeCapability) -> Result<ResolvedPolicy, PolicyError> {
        let mut effective_tier = self.tier;

        // Degrade if necessary
        if !caps.supports_tier(effective_tier) {
            if self.allow_degradation {
                effective_tier = caps.best_tier();
            } else {
                return Err(PolicyError::CapabilityInsufficient {
                    requested: self.tier,
                    best_available: caps.best_tier(),
                });
            }
        }

        let line_h = self.effective_line_height();

        // Build the three subsystem configs from the effective tier
        let objective = match effective_tier {
            LayoutTier::Fast => ParagraphObjective::terminal(),
            LayoutTier::Balanced => ParagraphObjective::default(),
            LayoutTier::Quality => ParagraphObjective::typographic(),
        };

        let vertical_policy = self.vertical_override.unwrap_or(match effective_tier {
            LayoutTier::Fast => VerticalPolicy::Compact,
            LayoutTier::Balanced => VerticalPolicy::Readable,
            LayoutTier::Quality => VerticalPolicy::Typographic,
        });

        let vertical = vertical_policy.resolve(line_h);

        let mut justification = match effective_tier {
            LayoutTier::Fast => JustificationControl::TERMINAL,
            LayoutTier::Balanced => JustificationControl::READABLE,
            LayoutTier::Quality => JustificationControl::TYPOGRAPHIC,
        };

        // Apply justify mode override
        if let Some(mode) = self.justify_override {
            justification.mode = mode;
        }

        // Capability-driven adjustments
        if !caps.tracking_support {
            // Disable inter-character glue if renderer can't do it
            justification.char_space = crate::justification::GlueSpec::rigid(0);
        }

        if !caps.proportional_fonts {
            // Monospace: make all spaces rigid (1 cell)
            justification.word_space =
                crate::justification::GlueSpec::rigid(crate::justification::SUBCELL_SCALE);
            justification.sentence_space =
                crate::justification::GlueSpec::rigid(crate::justification::SUBCELL_SCALE);
            justification.char_space = crate::justification::GlueSpec::rigid(0);
        }

        let degraded = effective_tier != self.tier;

        Ok(ResolvedPolicy {
            requested_tier: self.tier,
            effective_tier,
            degraded,
            objective,
            vertical,
            justification,
            use_hyphenation: caps.hyphenation_available && effective_tier >= LayoutTier::Balanced,
            use_optimal_breaking: effective_tier >= LayoutTier::Balanced,
            line_height_subpx: line_h,
        })
    }
}

impl Default for LayoutPolicy {
    fn default() -> Self {
        Self::BALANCED
    }
}

impl fmt::Display for LayoutPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tier={} degrade={}", self.tier, self.allow_degradation)
    }
}

// =========================================================================
// ResolvedPolicy
// =========================================================================

/// Fully-resolved layout configuration ready for use by the layout engine.
///
/// All three subsystem configs are populated and compatible with the
/// available runtime capabilities.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPolicy {
    /// What the user originally requested.
    pub requested_tier: LayoutTier,
    /// What was actually activated (may differ if degraded).
    pub effective_tier: LayoutTier,
    /// Whether degradation occurred.
    pub degraded: bool,

    /// Knuth-Plass line-break tuning.
    pub objective: ParagraphObjective,
    /// Resolved vertical metrics (leading, spacing, grid).
    pub vertical: VerticalMetrics,
    /// Justification controls (stretch/shrink/penalties).
    pub justification: JustificationControl,

    /// Whether hyphenation should be used for line breaking.
    pub use_hyphenation: bool,
    /// Whether optimal (Knuth-Plass) breaking should be used.
    /// False = greedy wrapping.
    pub use_optimal_breaking: bool,
    /// Line height in sub-pixel units.
    pub line_height_subpx: u32,
}

impl ResolvedPolicy {
    /// Whether justification (space stretching) is active.
    #[must_use]
    pub fn is_justified(&self) -> bool {
        self.justification.mode.requires_justification()
    }

    /// Human-readable summary of what features are active.
    #[must_use]
    pub fn feature_summary(&self) -> Vec<&'static str> {
        let mut features = Vec::new();

        if self.use_optimal_breaking {
            features.push("optimal-breaking");
        } else {
            features.push("greedy-wrapping");
        }

        if self.is_justified() {
            features.push("justified");
        }

        if self.use_hyphenation {
            features.push("hyphenation");
        }

        if self.vertical.baseline_grid.is_active() {
            features.push("baseline-grid");
        }

        if self.vertical.first_line_indent_subpx > 0 {
            features.push("first-line-indent");
        }

        if !self.justification.char_space.is_rigid() {
            features.push("tracking");
        }

        features
    }
}

impl fmt::Display for ResolvedPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (requested {}{})",
            self.effective_tier,
            self.requested_tier,
            if self.degraded { ", degraded" } else { "" }
        )
    }
}

// =========================================================================
// PolicyError
// =========================================================================

/// Errors from policy resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// The requested tier cannot be supported and degradation is disabled.
    CapabilityInsufficient {
        /// What was requested.
        requested: LayoutTier,
        /// The best the runtime can do.
        best_available: LayoutTier,
    },
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapabilityInsufficient {
                requested,
                best_available,
            } => write!(
                f,
                "requested tier '{}' not supported; best available is '{}'",
                requested, best_available
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::justification::JustifyMode;
    use crate::vertical_metrics::VerticalPolicy;

    // ── LayoutTier ───────────────────────────────────────────────────

    #[test]
    fn tier_ordering() {
        assert!(LayoutTier::Fast < LayoutTier::Balanced);
        assert!(LayoutTier::Balanced < LayoutTier::Quality);
    }

    #[test]
    fn tier_degrade_quality() {
        assert_eq!(LayoutTier::Quality.degrade(), Some(LayoutTier::Balanced));
    }

    #[test]
    fn tier_degrade_balanced() {
        assert_eq!(LayoutTier::Balanced.degrade(), Some(LayoutTier::Fast));
    }

    #[test]
    fn tier_degrade_fast_is_none() {
        assert_eq!(LayoutTier::Fast.degrade(), None);
    }

    #[test]
    fn tier_degradation_chain_quality() {
        let chain = LayoutTier::Quality.degradation_chain();
        assert_eq!(
            chain,
            vec![LayoutTier::Quality, LayoutTier::Balanced, LayoutTier::Fast]
        );
    }

    #[test]
    fn tier_degradation_chain_fast() {
        let chain = LayoutTier::Fast.degradation_chain();
        assert_eq!(chain, vec![LayoutTier::Fast]);
    }

    #[test]
    fn tier_default_is_balanced() {
        assert_eq!(LayoutTier::default(), LayoutTier::Balanced);
    }

    #[test]
    fn tier_display() {
        assert_eq!(format!("{}", LayoutTier::Quality), "quality");
        assert_eq!(format!("{}", LayoutTier::Balanced), "balanced");
        assert_eq!(format!("{}", LayoutTier::Fast), "fast");
    }

    // ── RuntimeCapability ────────────────────────────────────────────

    #[test]
    fn terminal_caps_support_fast() {
        assert!(RuntimeCapability::TERMINAL.supports_tier(LayoutTier::Fast));
    }

    #[test]
    fn terminal_caps_support_balanced() {
        assert!(RuntimeCapability::TERMINAL.supports_tier(LayoutTier::Balanced));
    }

    #[test]
    fn terminal_caps_not_quality() {
        assert!(!RuntimeCapability::TERMINAL.supports_tier(LayoutTier::Quality));
    }

    #[test]
    fn full_caps_support_all() {
        assert!(RuntimeCapability::FULL.supports_tier(LayoutTier::Quality));
    }

    #[test]
    fn terminal_best_tier_is_balanced() {
        assert_eq!(
            RuntimeCapability::TERMINAL.best_tier(),
            LayoutTier::Balanced
        );
    }

    #[test]
    fn full_best_tier_is_quality() {
        assert_eq!(RuntimeCapability::FULL.best_tier(), LayoutTier::Quality);
    }

    #[test]
    fn web_best_tier_is_quality() {
        assert_eq!(RuntimeCapability::WEB.best_tier(), LayoutTier::Quality);
    }

    #[test]
    fn default_caps_are_terminal() {
        let caps = RuntimeCapability::default();
        assert!(!caps.proportional_fonts);
        assert!(!caps.subpixel_positioning);
    }

    #[test]
    fn capability_display() {
        let s = format!("{}", RuntimeCapability::FULL);
        assert!(s.contains("proportional=true"));
    }

    // ── LayoutPolicy resolve ─────────────────────────────────────────

    #[test]
    fn fast_resolves_with_terminal_caps() {
        let result = LayoutPolicy::FAST.resolve(&RuntimeCapability::TERMINAL);
        let resolved = result.unwrap();
        assert_eq!(resolved.effective_tier, LayoutTier::Fast);
        assert!(!resolved.degraded);
        assert!(!resolved.use_optimal_breaking);
    }

    #[test]
    fn balanced_resolves_with_terminal_caps() {
        let result = LayoutPolicy::BALANCED.resolve(&RuntimeCapability::TERMINAL);
        let resolved = result.unwrap();
        assert_eq!(resolved.effective_tier, LayoutTier::Balanced);
        assert!(!resolved.degraded);
        assert!(resolved.use_optimal_breaking);
    }

    #[test]
    fn quality_degrades_on_terminal() {
        let result = LayoutPolicy::QUALITY.resolve(&RuntimeCapability::TERMINAL);
        let resolved = result.unwrap();
        assert!(resolved.degraded);
        assert_eq!(resolved.effective_tier, LayoutTier::Balanced);
        assert_eq!(resolved.requested_tier, LayoutTier::Quality);
    }

    #[test]
    fn quality_resolves_with_full_caps() {
        let result = LayoutPolicy::QUALITY.resolve(&RuntimeCapability::FULL);
        let resolved = result.unwrap();
        assert_eq!(resolved.effective_tier, LayoutTier::Quality);
        assert!(!resolved.degraded);
        assert!(resolved.is_justified());
        assert!(resolved.use_hyphenation);
    }

    #[test]
    fn degradation_disabled_returns_error() {
        let policy = LayoutPolicy {
            tier: LayoutTier::Quality,
            allow_degradation: false,
            ..LayoutPolicy::QUALITY
        };
        let result = policy.resolve(&RuntimeCapability::TERMINAL);
        assert!(result.is_err());
        if let Err(PolicyError::CapabilityInsufficient {
            requested,
            best_available,
        }) = result
        {
            assert_eq!(requested, LayoutTier::Quality);
            assert_eq!(best_available, LayoutTier::Balanced);
        }
    }

    // ── Overrides ────────────────────────────────────────────────────

    #[test]
    fn justify_override_applied() {
        let policy = LayoutPolicy {
            justify_override: Some(JustifyMode::Center),
            ..LayoutPolicy::BALANCED
        };
        let resolved = policy.resolve(&RuntimeCapability::TERMINAL).unwrap();
        assert_eq!(resolved.justification.mode, JustifyMode::Center);
    }

    #[test]
    fn vertical_override_applied() {
        let policy = LayoutPolicy {
            vertical_override: Some(VerticalPolicy::Typographic),
            ..LayoutPolicy::FAST
        };
        let resolved = policy.resolve(&RuntimeCapability::TERMINAL).unwrap();
        // Typographic policy activates baseline grid
        assert!(resolved.vertical.baseline_grid.is_active());
    }

    #[test]
    fn custom_line_height() {
        let policy = LayoutPolicy {
            line_height_subpx: 20 * 256, // 20px
            ..LayoutPolicy::BALANCED
        };
        let resolved = policy.resolve(&RuntimeCapability::TERMINAL).unwrap();
        assert_eq!(resolved.line_height_subpx, 20 * 256);
    }

    #[test]
    fn default_line_height_is_16px() {
        let policy = LayoutPolicy::BALANCED;
        assert_eq!(policy.effective_line_height(), 16 * 256);
    }

    // ── Capability-driven adjustments ────────────────────────────────

    #[test]
    fn no_tracking_disables_char_space() {
        let caps = RuntimeCapability {
            proportional_fonts: true,
            tracking_support: false,
            ..RuntimeCapability::FULL
        };
        let resolved = LayoutPolicy::QUALITY.resolve(&caps).unwrap();
        assert!(resolved.justification.char_space.is_rigid());
    }

    #[test]
    fn monospace_makes_spaces_rigid() {
        let resolved = LayoutPolicy::BALANCED
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        assert!(resolved.justification.word_space.is_rigid());
        assert!(resolved.justification.sentence_space.is_rigid());
    }

    #[test]
    fn no_hyphenation_dict_disables_hyphenation() {
        let caps = RuntimeCapability {
            proportional_fonts: true,
            hyphenation_available: false,
            ..RuntimeCapability::FULL
        };
        let resolved = LayoutPolicy::QUALITY.resolve(&caps).unwrap();
        assert!(!resolved.use_hyphenation);
    }

    // ── ResolvedPolicy ───────────────────────────────────────────────

    #[test]
    fn fast_not_justified() {
        let resolved = LayoutPolicy::FAST
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        assert!(!resolved.is_justified());
    }

    #[test]
    fn quality_is_justified() {
        let resolved = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::FULL)
            .unwrap();
        assert!(resolved.is_justified());
    }

    #[test]
    fn feature_summary_fast() {
        let resolved = LayoutPolicy::FAST
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        let features = resolved.feature_summary();
        assert!(features.contains(&"greedy-wrapping"));
        assert!(!features.contains(&"justified"));
    }

    #[test]
    fn feature_summary_quality() {
        let resolved = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::FULL)
            .unwrap();
        let features = resolved.feature_summary();
        assert!(features.contains(&"optimal-breaking"));
        assert!(features.contains(&"justified"));
        assert!(features.contains(&"hyphenation"));
        assert!(features.contains(&"baseline-grid"));
        assert!(features.contains(&"first-line-indent"));
        assert!(features.contains(&"tracking"));
    }

    #[test]
    fn resolved_display_no_degradation() {
        let resolved = LayoutPolicy::BALANCED
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        let s = format!("{resolved}");
        assert!(s.contains("balanced"));
        assert!(!s.contains("degraded"));
    }

    #[test]
    fn resolved_display_with_degradation() {
        let resolved = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        let s = format!("{resolved}");
        assert!(s.contains("degraded"));
    }

    // ── PolicyError ──────────────────────────────────────────────────

    #[test]
    fn error_display() {
        let err = PolicyError::CapabilityInsufficient {
            requested: LayoutTier::Quality,
            best_available: LayoutTier::Fast,
        };
        let s = format!("{err}");
        assert!(s.contains("quality"));
        assert!(s.contains("fast"));
    }

    #[test]
    fn error_is_error_trait() {
        let err = PolicyError::CapabilityInsufficient {
            requested: LayoutTier::Quality,
            best_available: LayoutTier::Fast,
        };
        let _: &dyn std::error::Error = &err;
    }

    // ── Default ──────────────────────────────────────────────────────

    #[test]
    fn default_policy_is_balanced() {
        assert_eq!(LayoutPolicy::default(), LayoutPolicy::BALANCED);
    }

    #[test]
    fn policy_display() {
        let s = format!("{}", LayoutPolicy::QUALITY);
        assert!(s.contains("quality"));
    }

    // ── Determinism ──────────────────────────────────────────────────

    #[test]
    fn same_inputs_same_resolution() {
        let p1 = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::FULL)
            .unwrap();
        let p2 = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::FULL)
            .unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn same_degradation_same_result() {
        let p1 = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        let p2 = LayoutPolicy::QUALITY
            .resolve(&RuntimeCapability::TERMINAL)
            .unwrap();
        assert_eq!(p1, p2);
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn fast_with_full_caps_stays_fast() {
        let resolved = LayoutPolicy::FAST
            .resolve(&RuntimeCapability::FULL)
            .unwrap();
        assert_eq!(resolved.effective_tier, LayoutTier::Fast);
        assert!(!resolved.degraded);
    }

    #[test]
    fn quality_with_justify_left_override() {
        let policy = LayoutPolicy {
            justify_override: Some(JustifyMode::Left),
            ..LayoutPolicy::QUALITY
        };
        let resolved = policy.resolve(&RuntimeCapability::FULL).unwrap();
        assert!(!resolved.is_justified());
        // Still quality tier even though not justified
        assert_eq!(resolved.effective_tier, LayoutTier::Quality);
    }
}
