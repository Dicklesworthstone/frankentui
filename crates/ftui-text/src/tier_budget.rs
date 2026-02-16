//! Frame-time, memory, and queue budgets per quality tier.
//!
//! This module defines measurable performance envelopes for each
//! [`LayoutTier`]. The adaptive quality controller (see the runtime control
//! layer) reads these budgets to decide when to degrade or promote tiers.
//!
//! # Budget model
//!
//! Each tier has three budget axes:
//!
//! - **Frame budget** — maximum wall-clock time (µs) for a single
//!   render frame (layout + shaping + diff + present).
//! - **Memory budget** — peak transient allocation ceiling (bytes) for
//!   per-frame working memory (caches, scratch buffers, glyph tables).
//! - **Queue budget** — maximum depth of the deferred work queue
//!   (re-shape, re-wrap, incremental reflow jobs).
//!
//! # Feature toggles
//!
//! [`TierFeatures`] defines which subsystem features are active at each
//! tier. Features are monotonically enabled as the tier increases:
//! everything active at Emergency is also active at Fast, and so on.
//!
//! # Safety constraints
//!
//! [`SafetyInvariant`] lists properties that must hold regardless of the
//! current tier. These are never disabled by the adaptive controller.

use std::fmt;
use std::time::Duration;

use crate::layout_policy::LayoutTier;

// =========================================================================
// FrameBudget
// =========================================================================

/// Wall-clock time budget for a single render frame.
///
/// All values are in microseconds for sub-millisecond precision without
/// floating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBudget {
    /// Total frame budget (layout + shaping + diff + present).
    pub total_us: u64,
    /// Maximum time for the layout solver pass.
    pub layout_us: u64,
    /// Maximum time for text shaping (shaped or terminal path).
    pub shaping_us: u64,
    /// Maximum time for buffer-diff computation.
    pub diff_us: u64,
    /// Maximum time for the presenter (ANSI emit).
    pub present_us: u64,
    /// Headroom reserved for widget render + event dispatch + IO.
    pub headroom_us: u64,
}

impl FrameBudget {
    /// Budget for a target frame rate.
    ///
    /// Returns the per-frame total in microseconds.
    #[must_use]
    pub const fn from_fps(fps: u32) -> u64 {
        1_000_000 / fps as u64
    }

    /// Convert the total budget to a [`Duration`].
    #[must_use]
    pub const fn as_duration(&self) -> Duration {
        Duration::from_micros(self.total_us)
    }

    /// Sum of allocated sub-budgets (should equal total).
    #[must_use]
    pub const fn allocated(&self) -> u64 {
        self.layout_us + self.shaping_us + self.diff_us + self.present_us + self.headroom_us
    }

    /// Whether the sub-budgets are consistent with total.
    #[must_use]
    pub const fn is_consistent(&self) -> bool {
        self.allocated() == self.total_us
    }
}

impl fmt::Display for FrameBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}µs (layout={}µs shaping={}µs diff={}µs present={}µs headroom={}µs)",
            self.total_us,
            self.layout_us,
            self.shaping_us,
            self.diff_us,
            self.present_us,
            self.headroom_us
        )
    }
}

// =========================================================================
// MemoryBudget
// =========================================================================

/// Transient per-frame memory budget.
///
/// These are ceilings for scratch allocations that are live during a
/// single frame. Persistent caches (width cache, shaping cache) are
/// accounted separately in their own capacity configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget {
    /// Maximum bytes for shaping scratch (glyph buffers, cluster maps).
    pub shaping_bytes: usize,
    /// Maximum bytes for layout scratch (constraint vectors, flex splits).
    pub layout_bytes: usize,
    /// Maximum bytes for diff scratch (dirty bitmaps, change lists).
    pub diff_bytes: usize,
    /// Maximum entries in the width cache.
    pub width_cache_entries: usize,
    /// Maximum entries in the shaping cache.
    pub shaping_cache_entries: usize,
}

impl MemoryBudget {
    /// Total transient ceiling (shaping + layout + diff).
    #[must_use]
    pub const fn transient_total(&self) -> usize {
        self.shaping_bytes + self.layout_bytes + self.diff_bytes
    }
}

impl fmt::Display for MemoryBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "transient={}B (shaping={}B layout={}B diff={}B) caches: width={} shaping={}",
            self.transient_total(),
            self.shaping_bytes,
            self.layout_bytes,
            self.diff_bytes,
            self.width_cache_entries,
            self.shaping_cache_entries
        )
    }
}

// =========================================================================
// QueueBudget
// =========================================================================

/// Work-queue depth limits for deferred layout/shaping jobs.
///
/// When the queue exceeds these limits, the adaptive controller should
/// degrade to a lower tier to reduce incoming work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueBudget {
    /// Maximum pending re-shape jobs (text runs awaiting shaping).
    pub max_reshape_pending: usize,
    /// Maximum pending re-wrap jobs (paragraphs awaiting line-breaking).
    pub max_rewrap_pending: usize,
    /// Maximum pending incremental reflow jobs.
    pub max_reflow_pending: usize,
}

impl QueueBudget {
    /// Total maximum pending jobs across all queues.
    #[must_use]
    pub const fn total_max(&self) -> usize {
        self.max_reshape_pending + self.max_rewrap_pending + self.max_reflow_pending
    }
}

impl fmt::Display for QueueBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "reshape={} rewrap={} reflow={}",
            self.max_reshape_pending, self.max_rewrap_pending, self.max_reflow_pending
        )
    }
}

// =========================================================================
// TierBudget
// =========================================================================

/// Combined budget for a single quality tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierBudget {
    /// Which tier this budget applies to.
    pub tier: LayoutTier,
    /// Frame-time budget.
    pub frame: FrameBudget,
    /// Memory budget.
    pub memory: MemoryBudget,
    /// Queue-depth budget.
    pub queue: QueueBudget,
}

impl fmt::Display for TierBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] frame: {} | mem: {} | queue: {}",
            self.tier, self.frame, self.memory, self.queue
        )
    }
}

// =========================================================================
// TierFeatures
// =========================================================================

/// Feature toggles for a quality tier.
///
/// Each flag indicates whether a subsystem feature is active at this tier.
/// Features are monotonically enabled: if a feature is on at tier T, it is
/// also on at every tier above T.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierFeatures {
    /// Which tier these features apply to.
    pub tier: LayoutTier,

    // ── Text shaping ────────────────────────────────────────────────
    /// Use the shaped text path (HarfBuzz/rustybuzz) when available.
    pub shaped_text: bool,
    /// Use the terminal (ClusterMap) fallback path.
    pub terminal_fallback: bool,

    // ── Line breaking ───────────────────────────────────────────────
    /// Use Knuth-Plass optimal line breaking.
    pub optimal_breaking: bool,
    /// Use hyphenation for line breaking.
    pub hyphenation: bool,

    // ── Spacing / justification ─────────────────────────────────────
    /// Enable full justification (stretch/shrink word spaces).
    pub justification: bool,
    /// Enable inter-character tracking.
    pub tracking: bool,

    // ── Vertical metrics ────────────────────────────────────────────
    /// Activate baseline grid snapping.
    pub baseline_grid: bool,
    /// Apply paragraph spacing.
    pub paragraph_spacing: bool,
    /// Apply first-line indent.
    pub first_line_indent: bool,

    // ── Caching ─────────────────────────────────────────────────────
    /// Use the width cache.
    pub width_cache: bool,
    /// Use the shaping cache.
    pub shaping_cache: bool,

    // ── Rendering ───────────────────────────────────────────────────
    /// Use incremental (dirty-region) diff.
    pub incremental_diff: bool,
    /// Use sub-cell spacing (1/256 cell precision).
    pub subcell_spacing: bool,
}

impl TierFeatures {
    /// Human-readable list of active features.
    #[must_use]
    pub fn active_list(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.shaped_text {
            out.push("shaped-text");
        }
        if self.terminal_fallback {
            out.push("terminal-fallback");
        }
        if self.optimal_breaking {
            out.push("optimal-breaking");
        }
        if self.hyphenation {
            out.push("hyphenation");
        }
        if self.justification {
            out.push("justification");
        }
        if self.tracking {
            out.push("tracking");
        }
        if self.baseline_grid {
            out.push("baseline-grid");
        }
        if self.paragraph_spacing {
            out.push("paragraph-spacing");
        }
        if self.first_line_indent {
            out.push("first-line-indent");
        }
        if self.width_cache {
            out.push("width-cache");
        }
        if self.shaping_cache {
            out.push("shaping-cache");
        }
        if self.incremental_diff {
            out.push("incremental-diff");
        }
        if self.subcell_spacing {
            out.push("subcell-spacing");
        }
        out
    }
}

impl fmt::Display for TierFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.tier, self.active_list().join(", "))
    }
}

// =========================================================================
// SafetyInvariant
// =========================================================================

/// Properties that must hold regardless of the current quality tier.
///
/// The adaptive controller must never violate these invariants, even
/// under extreme compute pressure. They define the semantic floor
/// below which output is no longer meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SafetyInvariant {
    /// Every input character must appear in the output buffer.
    /// No content may be silently dropped.
    NoContentLoss,
    /// Wide characters (CJK, emoji) must occupy 2 cells.
    /// Displaying a wide char in 1 cell corrupts layout.
    WideCharWidth,
    /// Buffer dimensions must match the terminal size.
    /// Mismatched buffers cause garbled output.
    BufferSizeMatch,
    /// Cursor position must be within buffer bounds.
    CursorInBounds,
    /// Style resets must be emitted at line boundaries.
    /// Leaking styles across lines corrupts subsequent output.
    StyleBoundary,
    /// Diff output must be idempotent: applying the same diff twice
    /// produces the same result as applying it once.
    DiffIdempotence,
    /// Greedy wrapping must always be available as a fallback.
    /// If optimal breaking fails, greedy wrapping takes over.
    GreedyWrapFallback,
    /// Width measurement must be deterministic for the same input.
    WidthDeterminism,
}

impl SafetyInvariant {
    /// All safety invariants.
    pub const ALL: &'static [Self] = &[
        Self::NoContentLoss,
        Self::WideCharWidth,
        Self::BufferSizeMatch,
        Self::CursorInBounds,
        Self::StyleBoundary,
        Self::DiffIdempotence,
        Self::GreedyWrapFallback,
        Self::WidthDeterminism,
    ];
}

impl fmt::Display for SafetyInvariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoContentLoss => write!(f, "no-content-loss"),
            Self::WideCharWidth => write!(f, "wide-char-width"),
            Self::BufferSizeMatch => write!(f, "buffer-size-match"),
            Self::CursorInBounds => write!(f, "cursor-in-bounds"),
            Self::StyleBoundary => write!(f, "style-boundary"),
            Self::DiffIdempotence => write!(f, "diff-idempotence"),
            Self::GreedyWrapFallback => write!(f, "greedy-wrap-fallback"),
            Self::WidthDeterminism => write!(f, "width-determinism"),
        }
    }
}

// =========================================================================
// TierLadder — the canonical budget/feature table
// =========================================================================

/// The canonical tier ladder: budgets, features, and safety constraints
/// for every quality tier.
///
/// This is the single source of truth that the adaptive controller reads
/// to decide degradation thresholds and feature availability.
#[derive(Debug, Clone)]
pub struct TierLadder {
    /// Budget for each tier, ordered Emergency → Fast → Balanced → Quality.
    pub budgets: [TierBudget; 4],
    /// Feature toggles for each tier, same order.
    pub features: [TierFeatures; 4],
}

impl TierLadder {
    /// Look up the budget for a specific tier.
    #[must_use]
    pub fn budget(&self, tier: LayoutTier) -> &TierBudget {
        &self.budgets[tier as usize]
    }

    /// Look up the feature toggles for a specific tier.
    #[must_use]
    pub fn features_for(&self, tier: LayoutTier) -> &TierFeatures {
        &self.features[tier as usize]
    }

    /// The default tier ladder calibrated from baseline profiles.
    ///
    /// Frame budgets target 60fps (16,667µs) with progressive headroom
    /// allocation. Memory budgets are sized for typical terminal
    /// workloads (80x24 to 200x60). Queue budgets prevent unbounded
    /// accumulation of deferred work.
    #[must_use]
    pub fn default_60fps() -> Self {
        Self {
            budgets: [
                // Emergency: 2ms total — survival mode
                TierBudget {
                    tier: LayoutTier::Emergency,
                    frame: FrameBudget {
                        total_us: 2_000,
                        layout_us: 100,
                        shaping_us: 200,
                        diff_us: 200,
                        present_us: 500,
                        headroom_us: 1_000,
                    },
                    memory: MemoryBudget {
                        shaping_bytes: 64 * 1024, // 64 KiB
                        layout_bytes: 16 * 1024,  // 16 KiB
                        diff_bytes: 32 * 1024,    // 32 KiB
                        width_cache_entries: 256,
                        shaping_cache_entries: 0, // disabled
                    },
                    queue: QueueBudget {
                        max_reshape_pending: 0, // no shaping
                        max_rewrap_pending: 4,  // minimal
                        max_reflow_pending: 1,
                    },
                },
                // Fast: 4ms total — terminal-optimized
                TierBudget {
                    tier: LayoutTier::Fast,
                    frame: FrameBudget {
                        total_us: 4_000,
                        layout_us: 200,
                        shaping_us: 800,
                        diff_us: 500,
                        present_us: 1_000,
                        headroom_us: 1_500,
                    },
                    memory: MemoryBudget {
                        shaping_bytes: 256 * 1024, // 256 KiB
                        layout_bytes: 64 * 1024,   // 64 KiB
                        diff_bytes: 128 * 1024,    // 128 KiB
                        width_cache_entries: 1_000,
                        shaping_cache_entries: 0, // no shaping at fast
                    },
                    queue: QueueBudget {
                        max_reshape_pending: 0, // no shaping
                        max_rewrap_pending: 16,
                        max_reflow_pending: 4,
                    },
                },
                // Balanced: 8ms total — good default
                TierBudget {
                    tier: LayoutTier::Balanced,
                    frame: FrameBudget {
                        total_us: 8_000,
                        layout_us: 500,
                        shaping_us: 2_500,
                        diff_us: 1_000,
                        present_us: 1_500,
                        headroom_us: 2_500,
                    },
                    memory: MemoryBudget {
                        shaping_bytes: 1024 * 1024, // 1 MiB
                        layout_bytes: 256 * 1024,   // 256 KiB
                        diff_bytes: 512 * 1024,     // 512 KiB
                        width_cache_entries: 4_000,
                        shaping_cache_entries: 512,
                    },
                    queue: QueueBudget {
                        max_reshape_pending: 32,
                        max_rewrap_pending: 64,
                        max_reflow_pending: 16,
                    },
                },
                // Quality: 16ms total — near full frame budget
                TierBudget {
                    tier: LayoutTier::Quality,
                    frame: FrameBudget {
                        total_us: 16_000,
                        layout_us: 1_000,
                        shaping_us: 5_000,
                        diff_us: 2_000,
                        present_us: 3_000,
                        headroom_us: 5_000,
                    },
                    memory: MemoryBudget {
                        shaping_bytes: 4 * 1024 * 1024, // 4 MiB
                        layout_bytes: 1024 * 1024,      // 1 MiB
                        diff_bytes: 2 * 1024 * 1024,    // 2 MiB
                        width_cache_entries: 16_000,
                        shaping_cache_entries: 2_048,
                    },
                    queue: QueueBudget {
                        max_reshape_pending: 128,
                        max_rewrap_pending: 256,
                        max_reflow_pending: 64,
                    },
                },
            ],
            features: [
                // Emergency
                TierFeatures {
                    tier: LayoutTier::Emergency,
                    shaped_text: false,
                    terminal_fallback: true,
                    optimal_breaking: false,
                    hyphenation: false,
                    justification: false,
                    tracking: false,
                    baseline_grid: false,
                    paragraph_spacing: false,
                    first_line_indent: false,
                    width_cache: true, // always on — cheap
                    shaping_cache: false,
                    incremental_diff: true, // always on — correctness aid
                    subcell_spacing: false,
                },
                // Fast
                TierFeatures {
                    tier: LayoutTier::Fast,
                    shaped_text: false,
                    terminal_fallback: true,
                    optimal_breaking: false,
                    hyphenation: false,
                    justification: false,
                    tracking: false,
                    baseline_grid: false,
                    paragraph_spacing: false,
                    first_line_indent: false,
                    width_cache: true,
                    shaping_cache: false,
                    incremental_diff: true,
                    subcell_spacing: false,
                },
                // Balanced
                TierFeatures {
                    tier: LayoutTier::Balanced,
                    shaped_text: true,
                    terminal_fallback: true,
                    optimal_breaking: true,
                    hyphenation: false,
                    justification: false,
                    tracking: false,
                    baseline_grid: false,
                    paragraph_spacing: true,
                    first_line_indent: false,
                    width_cache: true,
                    shaping_cache: true,
                    incremental_diff: true,
                    subcell_spacing: true,
                },
                // Quality
                TierFeatures {
                    tier: LayoutTier::Quality,
                    shaped_text: true,
                    terminal_fallback: true,
                    optimal_breaking: true,
                    hyphenation: true,
                    justification: true,
                    tracking: true,
                    baseline_grid: true,
                    paragraph_spacing: true,
                    first_line_indent: true,
                    width_cache: true,
                    shaping_cache: true,
                    incremental_diff: true,
                    subcell_spacing: true,
                },
            ],
        }
    }

    /// Verify that feature toggles are monotonically enabled up the ladder.
    ///
    /// Returns a list of violations where a higher tier disables a feature
    /// that a lower tier enables.
    #[must_use]
    pub fn check_monotonicity(&self) -> Vec<String> {
        let mut violations = Vec::new();

        for i in 0..self.features.len() - 1 {
            let lower = &self.features[i];
            let higher = &self.features[i + 1];

            let check = |name: &str, lo: bool, hi: bool| {
                if lo && !hi {
                    Some(format!(
                        "{name} is enabled at {} but disabled at {}",
                        lower.tier, higher.tier
                    ))
                } else {
                    None
                }
            };

            violations.extend(check(
                "terminal_fallback",
                lower.terminal_fallback,
                higher.terminal_fallback,
            ));
            violations.extend(check("width_cache", lower.width_cache, higher.width_cache));
            violations.extend(check(
                "incremental_diff",
                lower.incremental_diff,
                higher.incremental_diff,
            ));
            violations.extend(check("shaped_text", lower.shaped_text, higher.shaped_text));
            violations.extend(check(
                "optimal_breaking",
                lower.optimal_breaking,
                higher.optimal_breaking,
            ));
            violations.extend(check("hyphenation", lower.hyphenation, higher.hyphenation));
            violations.extend(check(
                "justification",
                lower.justification,
                higher.justification,
            ));
            violations.extend(check("tracking", lower.tracking, higher.tracking));
            violations.extend(check(
                "baseline_grid",
                lower.baseline_grid,
                higher.baseline_grid,
            ));
            violations.extend(check(
                "paragraph_spacing",
                lower.paragraph_spacing,
                higher.paragraph_spacing,
            ));
            violations.extend(check(
                "first_line_indent",
                lower.first_line_indent,
                higher.first_line_indent,
            ));
            violations.extend(check(
                "shaping_cache",
                lower.shaping_cache,
                higher.shaping_cache,
            ));
            violations.extend(check(
                "subcell_spacing",
                lower.subcell_spacing,
                higher.subcell_spacing,
            ));
        }
        violations
    }

    /// Verify that all budgets are consistent (sub-budgets sum to total).
    #[must_use]
    pub fn check_budget_consistency(&self) -> Vec<String> {
        let mut issues = Vec::new();
        for b in &self.budgets {
            if !b.frame.is_consistent() {
                issues.push(format!(
                    "[{}] frame sub-budgets sum to {}µs but total is {}µs",
                    b.tier,
                    b.frame.allocated(),
                    b.frame.total_us
                ));
            }
        }
        issues
    }

    /// Verify that budgets increase monotonically up the tier ladder.
    #[must_use]
    pub fn check_budget_ordering(&self) -> Vec<String> {
        let mut issues = Vec::new();
        for i in 0..self.budgets.len() - 1 {
            let lower = &self.budgets[i];
            let higher = &self.budgets[i + 1];
            if lower.frame.total_us >= higher.frame.total_us {
                issues.push(format!(
                    "frame budget {} ({}µs) >= {} ({}µs)",
                    lower.tier, lower.frame.total_us, higher.tier, higher.frame.total_us
                ));
            }
        }
        issues
    }
}

impl Default for TierLadder {
    fn default() -> Self {
        Self::default_60fps()
    }
}

impl fmt::Display for TierLadder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.budgets {
            writeln!(f, "{b}")?;
        }
        writeln!(f)?;
        for feat in &self.features {
            writeln!(f, "{feat}")?;
        }
        Ok(())
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── FrameBudget ─────────────────────────────────────────────────

    #[test]
    fn fps_60_is_16667us() {
        assert_eq!(FrameBudget::from_fps(60), 16_666);
    }

    #[test]
    fn fps_30_is_33333us() {
        assert_eq!(FrameBudget::from_fps(30), 33_333);
    }

    #[test]
    fn frame_budget_as_duration() {
        let fb = FrameBudget {
            total_us: 16_000,
            layout_us: 1_000,
            shaping_us: 5_000,
            diff_us: 2_000,
            present_us: 3_000,
            headroom_us: 5_000,
        };
        assert_eq!(fb.as_duration(), Duration::from_micros(16_000));
    }

    #[test]
    fn frame_budget_consistency() {
        let fb = FrameBudget {
            total_us: 10_000,
            layout_us: 1_000,
            shaping_us: 2_000,
            diff_us: 2_000,
            present_us: 2_000,
            headroom_us: 3_000,
        };
        assert!(fb.is_consistent());
    }

    #[test]
    fn frame_budget_inconsistency() {
        let fb = FrameBudget {
            total_us: 10_000,
            layout_us: 1_000,
            shaping_us: 2_000,
            diff_us: 2_000,
            present_us: 2_000,
            headroom_us: 999, // wrong
        };
        assert!(!fb.is_consistent());
    }

    #[test]
    fn frame_budget_display() {
        let fb = FrameBudget {
            total_us: 4_000,
            layout_us: 200,
            shaping_us: 800,
            diff_us: 500,
            present_us: 1_000,
            headroom_us: 1_500,
        };
        let s = format!("{fb}");
        assert!(s.contains("4000µs"));
        assert!(s.contains("layout=200µs"));
    }

    // ── MemoryBudget ────────────────────────────────────────────────

    #[test]
    fn memory_transient_total() {
        let mb = MemoryBudget {
            shaping_bytes: 1024,
            layout_bytes: 512,
            diff_bytes: 256,
            width_cache_entries: 100,
            shaping_cache_entries: 50,
        };
        assert_eq!(mb.transient_total(), 1792);
    }

    #[test]
    fn memory_budget_display() {
        let mb = MemoryBudget {
            shaping_bytes: 1024,
            layout_bytes: 512,
            diff_bytes: 256,
            width_cache_entries: 100,
            shaping_cache_entries: 50,
        };
        let s = format!("{mb}");
        assert!(s.contains("transient=1792B"));
    }

    // ── QueueBudget ─────────────────────────────────────────────────

    #[test]
    fn queue_total_max() {
        let qb = QueueBudget {
            max_reshape_pending: 10,
            max_rewrap_pending: 20,
            max_reflow_pending: 5,
        };
        assert_eq!(qb.total_max(), 35);
    }

    #[test]
    fn queue_budget_display() {
        let qb = QueueBudget {
            max_reshape_pending: 10,
            max_rewrap_pending: 20,
            max_reflow_pending: 5,
        };
        let s = format!("{qb}");
        assert!(s.contains("reshape=10"));
    }

    // ── TierBudget ──────────────────────────────────────────────────

    #[test]
    fn tier_budget_display() {
        let ladder = TierLadder::default_60fps();
        let s = format!("{}", ladder.budget(LayoutTier::Fast));
        assert!(s.contains("[fast]"));
        assert!(s.contains("4000µs"));
    }

    // ── TierFeatures ────────────────────────────────────────────────

    #[test]
    fn emergency_features_minimal() {
        let ladder = TierLadder::default_60fps();
        let f = ladder.features_for(LayoutTier::Emergency);
        assert!(!f.shaped_text);
        assert!(!f.optimal_breaking);
        assert!(!f.justification);
        assert!(!f.hyphenation);
        assert!(f.terminal_fallback);
        assert!(f.width_cache);
        assert!(f.incremental_diff);
    }

    #[test]
    fn fast_features() {
        let ladder = TierLadder::default_60fps();
        let f = ladder.features_for(LayoutTier::Fast);
        assert!(!f.shaped_text);
        assert!(!f.optimal_breaking);
        assert!(f.terminal_fallback);
        assert!(f.width_cache);
    }

    #[test]
    fn balanced_features() {
        let ladder = TierLadder::default_60fps();
        let f = ladder.features_for(LayoutTier::Balanced);
        assert!(f.shaped_text);
        assert!(f.optimal_breaking);
        assert!(f.shaping_cache);
        assert!(f.subcell_spacing);
        assert!(!f.hyphenation);
        assert!(!f.justification);
    }

    #[test]
    fn quality_features_all_on() {
        let ladder = TierLadder::default_60fps();
        let f = ladder.features_for(LayoutTier::Quality);
        assert!(f.shaped_text);
        assert!(f.optimal_breaking);
        assert!(f.hyphenation);
        assert!(f.justification);
        assert!(f.tracking);
        assert!(f.baseline_grid);
        assert!(f.first_line_indent);
    }

    #[test]
    fn feature_active_list() {
        let ladder = TierLadder::default_60fps();
        let list = ladder.features_for(LayoutTier::Emergency).active_list();
        assert!(list.contains(&"terminal-fallback"));
        assert!(list.contains(&"width-cache"));
        assert!(list.contains(&"incremental-diff"));
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn features_display() {
        let ladder = TierLadder::default_60fps();
        let s = format!("{}", ladder.features_for(LayoutTier::Quality));
        assert!(s.contains("[quality]"));
        assert!(s.contains("justification"));
    }

    // ── TierLadder ──────────────────────────────────────────────────

    #[test]
    fn default_ladder_budgets_are_consistent() {
        let ladder = TierLadder::default_60fps();
        let issues = ladder.check_budget_consistency();
        assert!(issues.is_empty(), "Budget inconsistencies: {issues:?}");
    }

    #[test]
    fn default_ladder_budgets_monotonically_increase() {
        let ladder = TierLadder::default_60fps();
        let issues = ladder.check_budget_ordering();
        assert!(issues.is_empty(), "Budget ordering violations: {issues:?}");
    }

    #[test]
    fn default_ladder_features_are_monotonic() {
        let ladder = TierLadder::default_60fps();
        let violations = ladder.check_monotonicity();
        assert!(
            violations.is_empty(),
            "Feature monotonicity violations: {violations:?}"
        );
    }

    #[test]
    fn ladder_budget_lookup() {
        let ladder = TierLadder::default_60fps();
        assert_eq!(ladder.budget(LayoutTier::Emergency).frame.total_us, 2_000);
        assert_eq!(ladder.budget(LayoutTier::Fast).frame.total_us, 4_000);
        assert_eq!(ladder.budget(LayoutTier::Balanced).frame.total_us, 8_000);
        assert_eq!(ladder.budget(LayoutTier::Quality).frame.total_us, 16_000);
    }

    #[test]
    fn ladder_display() {
        let ladder = TierLadder::default_60fps();
        let s = format!("{ladder}");
        assert!(s.contains("[emergency]"));
        assert!(s.contains("[fast]"));
        assert!(s.contains("[balanced]"));
        assert!(s.contains("[quality]"));
    }

    #[test]
    fn default_trait() {
        let ladder = TierLadder::default();
        assert_eq!(ladder.budget(LayoutTier::Fast).frame.total_us, 4_000);
    }

    // ── SafetyInvariant ─────────────────────────────────────────────

    #[test]
    fn all_invariants_listed() {
        assert_eq!(SafetyInvariant::ALL.len(), 8);
    }

    #[test]
    fn invariant_display() {
        assert_eq!(
            format!("{}", SafetyInvariant::NoContentLoss),
            "no-content-loss"
        );
        assert_eq!(
            format!("{}", SafetyInvariant::WideCharWidth),
            "wide-char-width"
        );
        assert_eq!(
            format!("{}", SafetyInvariant::GreedyWrapFallback),
            "greedy-wrap-fallback"
        );
    }

    #[test]
    fn invariants_cover_key_concerns() {
        let all = SafetyInvariant::ALL;
        assert!(all.contains(&SafetyInvariant::NoContentLoss));
        assert!(all.contains(&SafetyInvariant::WideCharWidth));
        assert!(all.contains(&SafetyInvariant::BufferSizeMatch));
        assert!(all.contains(&SafetyInvariant::DiffIdempotence));
        assert!(all.contains(&SafetyInvariant::WidthDeterminism));
    }

    // ── Integration: budget fits within 60fps ───────────────────────

    #[test]
    fn all_budgets_within_60fps() {
        let frame_budget_60fps = FrameBudget::from_fps(60);
        let ladder = TierLadder::default_60fps();
        for b in &ladder.budgets {
            assert!(
                b.frame.total_us <= frame_budget_60fps,
                "{} budget {}µs exceeds 60fps frame ({}µs)",
                b.tier,
                b.frame.total_us,
                frame_budget_60fps
            );
        }
    }

    #[test]
    fn emergency_queue_disables_reshape() {
        let ladder = TierLadder::default_60fps();
        assert_eq!(
            ladder
                .budget(LayoutTier::Emergency)
                .queue
                .max_reshape_pending,
            0
        );
    }

    #[test]
    fn quality_has_largest_caches() {
        let ladder = TierLadder::default_60fps();
        let e = &ladder.budget(LayoutTier::Emergency).memory;
        let q = &ladder.budget(LayoutTier::Quality).memory;
        assert!(q.width_cache_entries > e.width_cache_entries);
        assert!(q.shaping_cache_entries > e.shaping_cache_entries);
    }
}
