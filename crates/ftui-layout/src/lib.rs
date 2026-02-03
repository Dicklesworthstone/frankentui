#![forbid(unsafe_code)]

//! Layout primitives and solvers.
//!
//! This crate provides layout components for terminal UIs:
//!
//! - [`Flex`] - 1D constraint-based layout (rows or columns)
//! - [`Grid`] - 2D constraint-based layout with cell spanning
//! - [`Constraint`] - Size constraints (Fixed, Percentage, Min, Max, Ratio, FitContent)
//! - [`debug`] - Layout constraint debugging and introspection
//! - [`cache`] - Layout result caching for memoization
//!
//! # Intrinsic Sizing
//!
//! The layout system supports content-aware sizing via [`LayoutSizeHint`] and
//! [`Flex::split_with_measurer`]:
//!
//! ```ignore
//! use ftui_layout::{Flex, Constraint, LayoutSizeHint};
//!
//! let flex = Flex::horizontal()
//!     .constraints([Constraint::FitContent, Constraint::Fill]);
//!
//! let rects = flex.split_with_measurer(area, |idx, available| {
//!     match idx {
//!         0 => LayoutSizeHint { min: 5, preferred: 20, max: None },
//!         _ => LayoutSizeHint::ZERO,
//!     }
//! });
//! ```

pub mod cache;
pub mod debug;
pub mod grid;
#[cfg(test)]
mod repro_max_constraint;
pub mod responsive;

pub use cache::{CoherenceCache, CoherenceId, LayoutCache, LayoutCacheKey, LayoutCacheStats};
pub use ftui_core::geometry::{Rect, Sides, Size};
pub use grid::{Grid, GridArea, GridLayout};
pub use responsive::Responsive;
use std::cmp::min;

/// A constraint on the size of a layout area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Constraint {
    /// An exact size in cells.
    Fixed(u16),
    /// A percentage of the total available size (0.0 to 100.0).
    Percentage(f32),
    /// A minimum size in cells.
    Min(u16),
    /// A maximum size in cells.
    Max(u16),
    /// A ratio of the remaining space (numerator, denominator).
    Ratio(u32, u32),
    /// Fill remaining space (like Min(0) but semantically clearer).
    Fill,
    /// Size to fit content using widget's preferred size from [`LayoutSizeHint`].
    ///
    /// When used with [`Flex::split_with_measurer`], the measurer callback provides
    /// the size hints. Falls back to Fill behavior if no measurer is provided.
    FitContent,
    /// Fit content but clamp to explicit bounds.
    ///
    /// The allocated size will be between `min` and `max`, using the widget's
    /// preferred size when within range.
    FitContentBounded {
        /// Minimum allocation regardless of content size.
        min: u16,
        /// Maximum allocation regardless of content size.
        max: u16,
    },
    /// Use widget's minimum size (shrink-to-fit).
    ///
    /// Allocates only the minimum space the widget requires.
    FitMin,
}

/// Size hint returned by measurer callbacks for intrinsic sizing.
///
/// This is a 1D projection of a widget's size constraints along the layout axis.
/// Use with [`Flex::split_with_measurer`] for content-aware layouts.
///
/// # Example
///
/// ```
/// use ftui_layout::LayoutSizeHint;
///
/// // A label that needs 5-20 cells, ideally 15
/// let hint = LayoutSizeHint {
///     min: 5,
///     preferred: 15,
///     max: Some(20),
/// };
///
/// // Clamp allocation to hint bounds
/// assert_eq!(hint.clamp(10), 10); // Within range
/// assert_eq!(hint.clamp(3), 5);   // Below min
/// assert_eq!(hint.clamp(30), 20); // Above max
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutSizeHint {
    /// Minimum size (widget clips below this).
    pub min: u16,
    /// Preferred size (ideal for content).
    pub preferred: u16,
    /// Maximum useful size (None = unbounded).
    pub max: Option<u16>,
}

impl LayoutSizeHint {
    /// Zero hint (no minimum, no preferred, unbounded).
    pub const ZERO: Self = Self {
        min: 0,
        preferred: 0,
        max: None,
    };

    /// Create an exact size hint (min = preferred = max).
    #[inline]
    pub const fn exact(size: u16) -> Self {
        Self {
            min: size,
            preferred: size,
            max: Some(size),
        }
    }

    /// Create a hint with minimum and preferred size, unbounded max.
    #[inline]
    pub const fn at_least(min: u16, preferred: u16) -> Self {
        Self {
            min,
            preferred,
            max: None,
        }
    }

    /// Clamp a value to this hint's bounds.
    #[inline]
    pub fn clamp(&self, value: u16) -> u16 {
        let max = self.max.unwrap_or(u16::MAX);
        value.max(self.min).min(max)
    }
}

/// The direction to layout items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// Top to bottom.
    #[default]
    Vertical,
    /// Left to right.
    Horizontal,
}

/// Alignment of items within the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    /// Align items to the start (left/top).
    #[default]
    Start,
    /// Center items within available space.
    Center,
    /// Align items to the end (right/bottom).
    End,
    /// Distribute space evenly around each item.
    SpaceAround,
    /// Distribute space evenly between items (no outer space).
    SpaceBetween,
}

/// Responsive breakpoint tiers for terminal widths.
///
/// Ordered from smallest to largest. Each variant represents a width
/// range determined by [`Breakpoints`].
///
/// | Breakpoint | Default Min Width | Typical Use               |
/// |-----------|-------------------|---------------------------|
/// | `Xs`      | < 60 cols         | Minimal / ultra-narrow    |
/// | `Sm`      | 60–89 cols        | Compact layouts           |
/// | `Md`      | 90–119 cols       | Standard terminal width   |
/// | `Lg`      | 120–159 cols      | Wide terminals            |
/// | `Xl`      | 160+ cols         | Ultra-wide / tiled        |
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Breakpoint {
    /// Extra small: narrowest tier.
    Xs,
    /// Small: compact layouts.
    Sm,
    /// Medium: standard terminal width.
    Md,
    /// Large: wide terminals.
    Lg,
    /// Extra large: ultra-wide or tiled layouts.
    Xl,
}

impl Breakpoint {
    /// All breakpoints in ascending order.
    pub const ALL: [Breakpoint; 5] = [
        Breakpoint::Xs,
        Breakpoint::Sm,
        Breakpoint::Md,
        Breakpoint::Lg,
        Breakpoint::Xl,
    ];

    /// Ordinal index (0–4).
    #[inline]
    const fn index(self) -> u8 {
        match self {
            Breakpoint::Xs => 0,
            Breakpoint::Sm => 1,
            Breakpoint::Md => 2,
            Breakpoint::Lg => 3,
            Breakpoint::Xl => 4,
        }
    }

    /// Short label for display.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Breakpoint::Xs => "xs",
            Breakpoint::Sm => "sm",
            Breakpoint::Md => "md",
            Breakpoint::Lg => "lg",
            Breakpoint::Xl => "xl",
        }
    }
}

impl std::fmt::Display for Breakpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Breakpoint thresholds for responsive layouts.
///
/// Each field is the minimum width (in terminal columns) for that breakpoint.
/// Xs implicitly starts at width 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breakpoints {
    /// Minimum width for Sm.
    pub sm: u16,
    /// Minimum width for Md.
    pub md: u16,
    /// Minimum width for Lg.
    pub lg: u16,
    /// Minimum width for Xl.
    pub xl: u16,
}

impl Breakpoints {
    /// Default breakpoints: 60 / 90 / 120 / 160 columns.
    pub const DEFAULT: Self = Self {
        sm: 60,
        md: 90,
        lg: 120,
        xl: 160,
    };

    /// Create breakpoints with explicit thresholds.
    ///
    /// Values are sanitized to be monotonically non-decreasing.
    pub const fn new(sm: u16, md: u16, lg: u16) -> Self {
        let md = if md < sm { sm } else { md };
        let lg = if lg < md { md } else { lg };
        // Default xl to lg + 40 if not specified via new_with_xl.
        let xl = if lg + 40 > lg { lg + 40 } else { u16::MAX };
        Self { sm, md, lg, xl }
    }

    /// Create breakpoints with all four explicit thresholds.
    ///
    /// Values are sanitized to be monotonically non-decreasing.
    pub const fn new_with_xl(sm: u16, md: u16, lg: u16, xl: u16) -> Self {
        let md = if md < sm { sm } else { md };
        let lg = if lg < md { md } else { lg };
        let xl = if xl < lg { lg } else { xl };
        Self { sm, md, lg, xl }
    }

    /// Classify a width into a breakpoint bucket.
    #[inline]
    pub const fn classify_width(self, width: u16) -> Breakpoint {
        if width >= self.xl {
            Breakpoint::Xl
        } else if width >= self.lg {
            Breakpoint::Lg
        } else if width >= self.md {
            Breakpoint::Md
        } else if width >= self.sm {
            Breakpoint::Sm
        } else {
            Breakpoint::Xs
        }
    }

    /// Classify a Size (uses width).
    #[inline]
    pub const fn classify_size(self, size: Size) -> Breakpoint {
        self.classify_width(size.width)
    }

    /// Check if width is at least a given breakpoint.
    #[inline]
    pub const fn at_least(self, width: u16, min: Breakpoint) -> bool {
        self.classify_width(width).index() >= min.index()
    }

    /// Check if width is between two breakpoints (inclusive).
    #[inline]
    pub const fn between(self, width: u16, min: Breakpoint, max: Breakpoint) -> bool {
        let idx = self.classify_width(width).index();
        idx >= min.index() && idx <= max.index()
    }

    /// Get the minimum width threshold for a given breakpoint.
    #[must_use]
    pub const fn threshold(self, bp: Breakpoint) -> u16 {
        match bp {
            Breakpoint::Xs => 0,
            Breakpoint::Sm => self.sm,
            Breakpoint::Md => self.md,
            Breakpoint::Lg => self.lg,
            Breakpoint::Xl => self.xl,
        }
    }

    /// Get all thresholds as `(Breakpoint, min_width)` pairs.
    #[must_use]
    pub const fn thresholds(self) -> [(Breakpoint, u16); 5] {
        [
            (Breakpoint::Xs, 0),
            (Breakpoint::Sm, self.sm),
            (Breakpoint::Md, self.md),
            (Breakpoint::Lg, self.lg),
            (Breakpoint::Xl, self.xl),
        ]
    }
}

/// Size negotiation hints for layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct Measurement {
    /// Minimum width in columns.
    pub min_width: u16,
    /// Minimum height in rows.
    pub min_height: u16,
    /// Maximum width (None = unbounded).
    pub max_width: Option<u16>,
    /// Maximum height (None = unbounded).
    pub max_height: Option<u16>,
}

impl Measurement {
    /// Create a fixed-size measurement (min == max).
    pub fn fixed(width: u16, height: u16) -> Self {
        Self {
            min_width: width,
            min_height: height,
            max_width: Some(width),
            max_height: Some(height),
        }
    }

    /// Create a flexible measurement with minimum size and no maximum.
    pub fn flexible(min_width: u16, min_height: u16) -> Self {
        Self {
            min_width,
            min_height,
            max_width: None,
            max_height: None,
        }
    }
}

/// A flexible layout container.
#[derive(Debug, Clone, Default)]
pub struct Flex {
    direction: Direction,
    constraints: Vec<Constraint>,
    margin: Sides,
    gap: u16,
    alignment: Alignment,
}

impl Flex {
    /// Create a new vertical flex layout.
    pub fn vertical() -> Self {
        Self {
            direction: Direction::Vertical,
            ..Default::default()
        }
    }

    /// Create a new horizontal flex layout.
    pub fn horizontal() -> Self {
        Self {
            direction: Direction::Horizontal,
            ..Default::default()
        }
    }

    /// Set the layout direction.
    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Set the constraints.
    pub fn constraints(mut self, constraints: impl IntoIterator<Item = Constraint>) -> Self {
        self.constraints = constraints.into_iter().collect();
        self
    }

    /// Set the margin.
    pub fn margin(mut self, margin: Sides) -> Self {
        self.margin = margin;
        self
    }

    /// Set the gap between items.
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Set the alignment.
    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Split the given area into smaller rectangles according to the configuration.
    pub fn split(&self, area: Rect) -> Vec<Rect> {
        // Apply margin
        let inner = area.inner(self.margin);
        if inner.is_empty() {
            return self.constraints.iter().map(|_| Rect::default()).collect();
        }

        let total_size = match self.direction {
            Direction::Horizontal => inner.width,
            Direction::Vertical => inner.height,
        };

        let count = self.constraints.len();
        if count == 0 {
            return Vec::new();
        }

        // Calculate gaps
        let total_gap = self.gap.saturating_mul((count - 1) as u16);
        let available_size = total_size.saturating_sub(total_gap);

        // Solve constraints to get sizes
        let sizes = solve_constraints(&self.constraints, available_size);

        // Convert sizes to rects
        self.sizes_to_rects(inner, &sizes)
    }

    fn sizes_to_rects(&self, area: Rect, sizes: &[u16]) -> Vec<Rect> {
        let mut rects = Vec::with_capacity(sizes.len());

        // Calculate total used space (sizes + gaps)
        let total_gaps = if sizes.len() > 1 {
            self.gap.saturating_mul((sizes.len() - 1) as u16)
        } else {
            0
        };
        let total_used: u16 = sizes.iter().sum::<u16>().saturating_add(total_gaps);
        let total_available = match self.direction {
            Direction::Horizontal => area.width,
            Direction::Vertical => area.height,
        };
        let leftover = total_available.saturating_sub(total_used);

        // Calculate starting position and gap adjustment based on alignment
        let (start_offset, extra_gap) = match self.alignment {
            Alignment::Start => (0, 0),
            Alignment::End => (leftover, 0),
            Alignment::Center => (leftover / 2, 0),
            Alignment::SpaceBetween => (0, 0),
            Alignment::SpaceAround => {
                if sizes.is_empty() {
                    (0, 0)
                } else {
                    // Space around: equal space before, between, and after
                    let space_unit = leftover / (sizes.len() as u16 * 2);
                    (space_unit, 0)
                }
            }
        };

        let mut current_pos = match self.direction {
            Direction::Horizontal => area.x.saturating_add(start_offset),
            Direction::Vertical => area.y.saturating_add(start_offset),
        };

        for (i, &size) in sizes.iter().enumerate() {
            let rect = match self.direction {
                Direction::Horizontal => Rect {
                    x: current_pos,
                    y: area.y,
                    width: size,
                    height: area.height,
                },
                Direction::Vertical => Rect {
                    x: area.x,
                    y: current_pos,
                    width: area.width,
                    height: size,
                },
            };
            rects.push(rect);

            // Advance position for next item
            current_pos = current_pos
                .saturating_add(size)
                .saturating_add(self.gap)
                .saturating_add(extra_gap);

            // Add alignment-specific spacing
            match self.alignment {
                Alignment::SpaceBetween => {
                    if sizes.len() > 1 && i < sizes.len() - 1 {
                        let count = (sizes.len() - 1) as u16;
                        let base = leftover / count;
                        let rem = leftover % count;
                        let extra = base + if (i as u16) < rem { 1 } else { 0 };
                        current_pos = current_pos.saturating_add(extra);
                    }
                }
                Alignment::SpaceAround => {
                    if !sizes.is_empty() {
                        let slots = sizes.len() as u16 * 2;
                        let unit = leftover / slots;
                        current_pos = current_pos.saturating_add(unit.saturating_mul(2));
                    }
                }
                _ => {}
            }
        }

        rects
    }

    /// Split area using intrinsic sizing from a measurer callback.
    ///
    /// This method enables content-aware layout with [`Constraint::FitContent`],
    /// [`Constraint::FitContentBounded`], and [`Constraint::FitMin`].
    ///
    /// # Arguments
    ///
    /// - `area`: Available rectangle
    /// - `measurer`: Callback that returns [`LayoutSizeHint`] for item at index
    ///
    /// # Example
    ///
    /// ```ignore
    /// let flex = Flex::horizontal()
    ///     .constraints([Constraint::FitContent, Constraint::Fill]);
    ///
    /// let rects = flex.split_with_measurer(area, |idx, available| {
    ///     match idx {
    ///         0 => LayoutSizeHint { min: 5, preferred: 20, max: None },
    ///         _ => LayoutSizeHint::ZERO,
    ///     }
    /// });
    /// ```
    pub fn split_with_measurer<F>(&self, area: Rect, measurer: F) -> Vec<Rect>
    where
        F: Fn(usize, u16) -> LayoutSizeHint,
    {
        // Apply margin
        let inner = area.inner(self.margin);
        if inner.is_empty() {
            return self.constraints.iter().map(|_| Rect::default()).collect();
        }

        let total_size = match self.direction {
            Direction::Horizontal => inner.width,
            Direction::Vertical => inner.height,
        };

        let count = self.constraints.len();
        if count == 0 {
            return Vec::new();
        }

        // Calculate gaps
        let total_gap = self.gap.saturating_mul((count - 1) as u16);
        let available_size = total_size.saturating_sub(total_gap);

        // Solve constraints with hints from measurer
        let sizes = solve_constraints_with_hints(&self.constraints, available_size, &measurer);

        // Convert sizes to rects
        self.sizes_to_rects(inner, &sizes)
    }
}

/// Solve 1D constraints to determine sizes.
///
/// This shared logic is used by both Flex and Grid layouts.
/// For intrinsic sizing support, use [`solve_constraints_with_hints`].
pub(crate) fn solve_constraints(constraints: &[Constraint], available_size: u16) -> Vec<u16> {
    // Use the with_hints version with a no-op measurer
    solve_constraints_with_hints(constraints, available_size, &|_, _| LayoutSizeHint::ZERO)
}

/// Solve 1D constraints with intrinsic sizing support.
///
/// The measurer callback provides size hints for FitContent, FitContentBounded, and FitMin
/// constraints. It receives the constraint index and remaining available space.
pub(crate) fn solve_constraints_with_hints<F>(
    constraints: &[Constraint],
    available_size: u16,
    measurer: &F,
) -> Vec<u16>
where
    F: Fn(usize, u16) -> LayoutSizeHint,
{
    let mut sizes = vec![0u16; constraints.len()];
    let mut remaining = available_size;
    let mut grow_indices = Vec::new();

    // 1. First pass: Allocate Fixed, Percentage, Min, and intrinsic sizing constraints
    for (i, &constraint) in constraints.iter().enumerate() {
        match constraint {
            Constraint::Fixed(size) => {
                let size = min(size, remaining);
                sizes[i] = size;
                remaining = remaining.saturating_sub(size);
            }
            Constraint::Percentage(p) => {
                let size = (available_size as f32 * p / 100.0)
                    .round()
                    .min(u16::MAX as f32) as u16;
                let size = min(size, remaining);
                sizes[i] = size;
                remaining = remaining.saturating_sub(size);
            }
            Constraint::Min(min_size) => {
                let size = min(min_size, remaining);
                sizes[i] = size;
                remaining = remaining.saturating_sub(size);
                grow_indices.push(i);
            }
            Constraint::Max(_) => {
                // Max initially takes 0, but is a candidate for growth
                grow_indices.push(i);
            }
            Constraint::Ratio(_, _) => {
                // Ratio takes 0 initially, candidate for growth
                grow_indices.push(i);
            }
            Constraint::Fill => {
                // Fill takes 0 initially, candidate for growth
                grow_indices.push(i);
            }
            Constraint::FitContent => {
                // Use measurer to get preferred size
                let hint = measurer(i, remaining);
                let size = min(hint.preferred, remaining);
                sizes[i] = size;
                remaining = remaining.saturating_sub(size);
                // FitContent items don't grow beyond preferred
            }
            Constraint::FitContentBounded {
                min: min_bound,
                max: max_bound,
            } => {
                // Use measurer to get preferred size, clamped to bounds
                let hint = measurer(i, remaining);
                let preferred = hint.preferred.max(min_bound).min(max_bound);
                let size = min(preferred, remaining);
                sizes[i] = size;
                remaining = remaining.saturating_sub(size);
            }
            Constraint::FitMin => {
                // Use measurer to get minimum size
                let hint = measurer(i, remaining);
                let size = min(hint.min, remaining);
                sizes[i] = size;
                remaining = remaining.saturating_sub(size);
                // FitMin items can grow to fill remaining space
                grow_indices.push(i);
            }
        }
    }

    // 2. Iterative distribution to flexible constraints
    loop {
        if remaining == 0 || grow_indices.is_empty() {
            break;
        }

        let mut total_weight = 0u64;
        const WEIGHT_SCALE: u64 = 10_000;

        for &i in &grow_indices {
            match constraints[i] {
                Constraint::Ratio(n, d) => {
                    total_weight += n as u64 * WEIGHT_SCALE / d.max(1) as u64
                }
                _ => total_weight += WEIGHT_SCALE,
            }
        }

        if total_weight == 0 {
            total_weight = 1;
        }

        let space_to_distribute = remaining;
        let mut allocated = 0;
        let mut shares = vec![0u16; constraints.len()];

        for (idx, &i) in grow_indices.iter().enumerate() {
            let weight = match constraints[i] {
                Constraint::Ratio(n, d) => n as u64 * WEIGHT_SCALE / d.max(1) as u64,
                _ => WEIGHT_SCALE,
            };

            // Last item gets the rest to ensure exact sum
            let size = if idx == grow_indices.len() - 1 {
                space_to_distribute - allocated
            } else {
                let s = (space_to_distribute as u64 * weight / total_weight) as u16;
                min(s, space_to_distribute - allocated)
            };

            shares[i] = size;
            allocated += size;
        }

        // Check for Max constraint violations
        let mut violations = Vec::new();
        for &i in &grow_indices {
            if let Constraint::Max(max_val) = constraints[i]
                && sizes[i].saturating_add(shares[i]) > max_val
            {
                violations.push(i);
            }
        }

        if violations.is_empty() {
            // No violations, commit shares and exit
            for &i in &grow_indices {
                sizes[i] = sizes[i].saturating_add(shares[i]);
            }
            break;
        }

        // Handle violations: clamp to Max and remove from grow pool
        for i in violations {
            if let Constraint::Max(max_val) = constraints[i] {
                // Calculate how much space this item *actually* consumes from remaining
                // which is (max - current_size)
                let consumed = max_val.saturating_sub(sizes[i]);
                sizes[i] = max_val;
                remaining = remaining.saturating_sub(consumed);

                // Remove from grow indices
                if let Some(pos) = grow_indices.iter().position(|&x| x == i) {
                    grow_indices.remove(pos);
                }
            }
        }
    }

    sizes
}

// ---------------------------------------------------------------------------
// Stable Layout Rounding: Min-Displacement with Temporal Coherence
// ---------------------------------------------------------------------------

/// Previous frame's allocation, used as tie-breaker for temporal stability.
///
/// Pass `None` for the first frame or when no history is available.
/// When provided, the rounding algorithm prefers allocations that
/// minimize change from the previous frame, reducing visual jitter.
pub type PreviousAllocation = Option<Vec<u16>>;

/// Round real-valued layout targets to integer cells with exact sum conservation.
///
/// # Mathematical Model
///
/// Given real-valued targets `r_i` (from the constraint solver) and a required
/// integer total, find integer allocations `x_i` that:
///
/// ```text
/// minimize   Σ_i |x_i − r_i|  +  μ · Σ_i |x_i − x_i_prev|
/// subject to Σ_i x_i = total
///            x_i ≥ 0
/// ```
///
/// where `x_i_prev` is the previous frame's allocation and `μ` is the temporal
/// stability weight (default 0.1).
///
/// # Algorithm: Largest Remainder with Temporal Tie-Breaking
///
/// This uses a variant of the Largest Remainder Method (Hamilton's method),
/// which provides optimal bounded displacement (|x_i − r_i| < 1 for all i):
///
/// 1. **Floor phase**: Set `x_i = floor(r_i)` for each element.
/// 2. **Deficit**: Compute `D = total − Σ floor(r_i)` extra cells to distribute.
/// 3. **Priority sort**: Rank elements by remainder `r_i − floor(r_i)` (descending).
///    Break ties using a composite key:
///    a. Prefer elements where `x_i_prev = ceil(r_i)` (temporal stability).
///    b. Prefer elements with smaller index (determinism).
/// 4. **Distribute**: Award one extra cell to each of the top `D` elements.
///
/// # Properties
///
/// 1. **Sum conservation**: `Σ x_i = total` exactly (proven by construction).
/// 2. **Bounded displacement**: `|x_i − r_i| < 1` for all `i` (since each x_i
///    is either `floor(r_i)` or `ceil(r_i)`).
/// 3. **Deterministic**: Same inputs → identical outputs (temporal tie-break +
///    index tie-break provide total ordering).
/// 4. **Temporal coherence**: When targets change slightly, allocations tend to
///    stay the same (preferring the previous frame's rounding direction).
/// 5. **Optimal displacement**: Among all integer allocations summing to `total`
///    with `floor(r_i) ≤ x_i ≤ ceil(r_i)`, the Largest Remainder Method
///    minimizes total absolute displacement.
///
/// # Failure Modes
///
/// - **All-zero targets**: Returns all zeros. Harmless (empty layout).
/// - **Negative deficit**: Can occur if targets sum to less than `total` after
///   flooring. The algorithm handles this via the clamp in step 2.
/// - **Very large N**: O(N log N) due to sorting. Acceptable for typical
///   layout counts (< 100 items).
///
/// # Example
///
/// ```
/// use ftui_layout::round_layout_stable;
///
/// // Targets: [10.4, 20.6, 9.0] must sum to 40
/// let result = round_layout_stable(&[10.4, 20.6, 9.0], 40, None);
/// assert_eq!(result.iter().sum::<u16>(), 40);
/// // 10.4 → 10, 20.6 → 21, 9.0 → 9 = 40 ✓
/// assert_eq!(result, vec![10, 21, 9]);
/// ```
pub fn round_layout_stable(targets: &[f64], total: u16, prev: PreviousAllocation) -> Vec<u16> {
    let n = targets.len();
    if n == 0 {
        return Vec::new();
    }

    // Step 1: Floor all targets
    let floors: Vec<u16> = targets
        .iter()
        .map(|&r| (r.max(0.0).floor() as u64).min(u16::MAX as u64) as u16)
        .collect();

    let floor_sum: u16 = floors.iter().copied().sum();

    // Step 2: Compute deficit (extra cells to distribute)
    let deficit = total.saturating_sub(floor_sum);

    if deficit == 0 {
        // Exact fit — no rounding needed
        // But we may need to adjust if floor_sum > total (overflow case)
        if floor_sum > total {
            return redistribute_overflow(&floors, total);
        }
        return floors;
    }

    // Step 3: Compute remainders and build priority list
    let mut priority: Vec<(usize, f64, bool)> = targets
        .iter()
        .enumerate()
        .map(|(i, &r)| {
            let remainder = r - (floors[i] as f64);
            let ceil_val = floors[i].saturating_add(1);
            // Temporal stability: did previous allocation use ceil?
            let prev_used_ceil = prev
                .as_ref()
                .is_some_and(|p| p.get(i).copied() == Some(ceil_val));
            (i, remainder, prev_used_ceil)
        })
        .collect();

    // Sort by: remainder descending, then temporal preference, then index ascending
    priority.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                // Prefer items where prev used ceil (true > false)
                b.2.cmp(&a.2)
            })
            .then_with(|| {
                // Deterministic tie-break: smaller index first
                a.0.cmp(&b.0)
            })
    });

    // Step 4: Distribute deficit
    let mut result = floors;
    let distribute = (deficit as usize).min(n);
    for &(i, _, _) in priority.iter().take(distribute) {
        result[i] = result[i].saturating_add(1);
    }

    result
}

/// Handle the edge case where floored values exceed total.
///
/// This can happen with very small totals and many items. We greedily
/// reduce the largest items by 1 until the sum matches.
fn redistribute_overflow(floors: &[u16], total: u16) -> Vec<u16> {
    let mut result = floors.to_vec();
    let mut current_sum: u16 = result.iter().copied().sum();

    // Build a max-heap of (value, index) to reduce largest first
    while current_sum > total {
        // Find the largest element
        if let Some((idx, _)) = result
            .iter()
            .enumerate()
            .filter(|item| *item.1 > 0)
            .max_by_key(|item| *item.1)
        {
            result[idx] = result[idx].saturating_sub(1);
            current_sum = current_sum.saturating_sub(1);
        } else {
            break;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_split() {
        let flex = Flex::horizontal().constraints([Constraint::Fixed(10), Constraint::Fixed(20)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], Rect::new(0, 0, 10, 10));
        assert_eq!(rects[1], Rect::new(10, 0, 20, 10)); // Gap is 0 by default
    }

    #[test]
    fn percentage_split() {
        let flex = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects[0].width, 50);
        assert_eq!(rects[1].width, 50);
    }

    #[test]
    fn gap_handling() {
        let flex = Flex::horizontal()
            .gap(5)
            .constraints([Constraint::Fixed(10), Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // Item 1: 0..10
        // Gap: 10..15
        // Item 2: 15..25
        assert_eq!(rects[0], Rect::new(0, 0, 10, 10));
        assert_eq!(rects[1], Rect::new(15, 0, 10, 10));
    }

    #[test]
    fn mixed_constraints() {
        let flex = Flex::horizontal().constraints([
            Constraint::Fixed(10),
            Constraint::Min(10), // Should take half of remaining (90/2 = 45) + base 10? No, logic is simplified.
            Constraint::Percentage(10.0), // 10% of 100 = 10
        ]);

        // Available: 100
        // Fixed(10) -> 10. Rem: 90.
        // Percent(10%) -> 10. Rem: 80.
        // Min(10) -> 10. Rem: 70.
        // Grow candidates: Min(10).
        // Distribute 70 to Min(10). Size = 10 + 70 = 80.

        let rects = flex.split(Rect::new(0, 0, 100, 1));
        assert_eq!(rects[0].width, 10); // Fixed
        assert_eq!(rects[2].width, 10); // Percent
        assert_eq!(rects[1].width, 80); // Min + Remainder
    }

    #[test]
    fn measurement_fixed_constraints() {
        let fixed = Measurement::fixed(5, 7);
        assert_eq!(fixed.min_width, 5);
        assert_eq!(fixed.min_height, 7);
        assert_eq!(fixed.max_width, Some(5));
        assert_eq!(fixed.max_height, Some(7));
    }

    #[test]
    fn measurement_flexible_constraints() {
        let flexible = Measurement::flexible(2, 3);
        assert_eq!(flexible.min_width, 2);
        assert_eq!(flexible.min_height, 3);
        assert_eq!(flexible.max_width, None);
        assert_eq!(flexible.max_height, None);
    }

    #[test]
    fn breakpoints_classify_defaults() {
        let bp = Breakpoints::DEFAULT;
        assert_eq!(bp.classify_width(20), Breakpoint::Xs);
        assert_eq!(bp.classify_width(60), Breakpoint::Sm);
        assert_eq!(bp.classify_width(90), Breakpoint::Md);
        assert_eq!(bp.classify_width(120), Breakpoint::Lg);
    }

    #[test]
    fn breakpoints_at_least_and_between() {
        let bp = Breakpoints::new(50, 80, 110);
        assert!(bp.at_least(85, Breakpoint::Sm));
        assert!(bp.between(85, Breakpoint::Sm, Breakpoint::Md));
        assert!(!bp.between(85, Breakpoint::Lg, Breakpoint::Lg));
    }

    #[test]
    fn alignment_end() {
        let flex = Flex::horizontal()
            .alignment(Alignment::End)
            .constraints([Constraint::Fixed(10), Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // Items should be pushed to the end: leftover = 100 - 20 = 80
        assert_eq!(rects[0], Rect::new(80, 0, 10, 10));
        assert_eq!(rects[1], Rect::new(90, 0, 10, 10));
    }

    #[test]
    fn alignment_center() {
        let flex = Flex::horizontal()
            .alignment(Alignment::Center)
            .constraints([Constraint::Fixed(20), Constraint::Fixed(20)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // Items should be centered: leftover = 100 - 40 = 60, offset = 30
        assert_eq!(rects[0], Rect::new(30, 0, 20, 10));
        assert_eq!(rects[1], Rect::new(50, 0, 20, 10));
    }

    #[test]
    fn alignment_space_between() {
        let flex = Flex::horizontal()
            .alignment(Alignment::SpaceBetween)
            .constraints([
                Constraint::Fixed(10),
                Constraint::Fixed(10),
                Constraint::Fixed(10),
            ]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // Items: 30 total, leftover = 70, 2 gaps, 35 per gap
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[1].x, 45); // 10 + 35
        assert_eq!(rects[2].x, 90); // 45 + 10 + 35
    }

    #[test]
    fn vertical_alignment() {
        let flex = Flex::vertical()
            .alignment(Alignment::End)
            .constraints([Constraint::Fixed(5), Constraint::Fixed(5)]);
        let rects = flex.split(Rect::new(0, 0, 10, 100));
        // Vertical: leftover = 100 - 10 = 90
        assert_eq!(rects[0], Rect::new(0, 90, 10, 5));
        assert_eq!(rects[1], Rect::new(0, 95, 10, 5));
    }

    #[test]
    fn nested_flex_support() {
        // Outer horizontal split
        let outer = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)]);
        let outer_rects = outer.split(Rect::new(0, 0, 100, 100));

        // Inner vertical split on the first half
        let inner = Flex::vertical().constraints([Constraint::Fixed(30), Constraint::Min(10)]);
        let inner_rects = inner.split(outer_rects[0]);

        assert_eq!(inner_rects[0], Rect::new(0, 0, 50, 30));
        assert_eq!(inner_rects[1], Rect::new(0, 30, 50, 70));
    }

    // Property-like invariant tests
    #[test]
    fn invariant_total_size_does_not_exceed_available() {
        // Test that constraint solving never allocates more than available
        for total in [10u16, 50, 100, 255] {
            let flex = Flex::horizontal().constraints([
                Constraint::Fixed(30),
                Constraint::Percentage(50.0),
                Constraint::Min(20),
            ]);
            let rects = flex.split(Rect::new(0, 0, total, 10));
            let total_width: u16 = rects.iter().map(|r| r.width).sum();
            assert!(
                total_width <= total,
                "Total width {} exceeded available {} for constraints",
                total_width,
                total
            );
        }
    }

    #[test]
    fn invariant_empty_area_produces_empty_rects() {
        let flex = Flex::horizontal().constraints([Constraint::Fixed(10), Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 0, 0));
        assert!(rects.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn invariant_no_constraints_produces_empty_vec() {
        let flex = Flex::horizontal().constraints([]);
        let rects = flex.split(Rect::new(0, 0, 100, 100));
        assert!(rects.is_empty());
    }

    // --- Ratio constraint ---

    #[test]
    fn ratio_constraint_splits_proportionally() {
        let flex =
            Flex::horizontal().constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)]);
        let rects = flex.split(Rect::new(0, 0, 90, 10));
        assert_eq!(rects[0].width, 30);
        assert_eq!(rects[1].width, 60);
    }

    #[test]
    fn ratio_constraint_with_zero_denominator() {
        // Zero denominator should not panic (max(1) guard)
        let flex = Flex::horizontal().constraints([Constraint::Ratio(1, 0)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects.len(), 1);
    }

    // --- Max constraint ---

    #[test]
    fn max_constraint_clamps_size() {
        let flex = Flex::horizontal().constraints([Constraint::Max(20), Constraint::Fixed(30)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert!(rects[0].width <= 20);
        assert_eq!(rects[1].width, 30);
    }

    // --- SpaceAround alignment ---

    #[test]
    fn alignment_space_around() {
        let flex = Flex::horizontal()
            .alignment(Alignment::SpaceAround)
            .constraints([Constraint::Fixed(10), Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));

        // SpaceAround: leftover = 80, space_unit = 80/(2*2) = 20
        // First item starts at 20, second at 20+10+40=70
        assert_eq!(rects[0].x, 20);
        assert_eq!(rects[1].x, 70);
    }

    // --- Vertical with gap ---

    #[test]
    fn vertical_gap() {
        let flex = Flex::vertical()
            .gap(5)
            .constraints([Constraint::Fixed(10), Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 50, 100));
        assert_eq!(rects[0], Rect::new(0, 0, 50, 10));
        assert_eq!(rects[1], Rect::new(0, 15, 50, 10));
    }

    // --- Vertical center alignment ---

    #[test]
    fn vertical_center() {
        let flex = Flex::vertical()
            .alignment(Alignment::Center)
            .constraints([Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 50, 100));
        // leftover = 90, offset = 45
        assert_eq!(rects[0].y, 45);
        assert_eq!(rects[0].height, 10);
    }

    // --- Single constraint gets all space ---

    #[test]
    fn single_min_takes_all() {
        let flex = Flex::horizontal().constraints([Constraint::Min(5)]);
        let rects = flex.split(Rect::new(0, 0, 80, 24));
        assert_eq!(rects[0].width, 80);
    }

    // --- Fixed exceeds available ---

    #[test]
    fn fixed_exceeds_available_clamped() {
        let flex = Flex::horizontal().constraints([Constraint::Fixed(60), Constraint::Fixed(60)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // First gets 60, second gets remaining 40 (clamped)
        assert_eq!(rects[0].width, 60);
        assert_eq!(rects[1].width, 40);
    }

    // --- Percentage that sums beyond 100% ---

    #[test]
    fn percentage_overflow_clamped() {
        let flex = Flex::horizontal()
            .constraints([Constraint::Percentage(80.0), Constraint::Percentage(80.0)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects[0].width, 80);
        assert_eq!(rects[1].width, 20); // clamped to remaining
    }

    // --- Margin reduces available space ---

    #[test]
    fn margin_reduces_split_area() {
        let flex = Flex::horizontal()
            .margin(Sides::all(10))
            .constraints([Constraint::Fixed(20), Constraint::Min(0)]);
        let rects = flex.split(Rect::new(0, 0, 100, 100));
        // Inner: 10,10,80,80
        assert_eq!(rects[0].x, 10);
        assert_eq!(rects[0].y, 10);
        assert_eq!(rects[0].width, 20);
        assert_eq!(rects[0].height, 80);
    }

    // --- Builder chain ---

    #[test]
    fn builder_methods_chain() {
        let flex = Flex::vertical()
            .direction(Direction::Horizontal)
            .gap(3)
            .margin(Sides::all(1))
            .alignment(Alignment::End)
            .constraints([Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 50, 50));
        assert_eq!(rects.len(), 1);
    }

    // --- SpaceBetween with single item ---

    #[test]
    fn space_between_single_item() {
        let flex = Flex::horizontal()
            .alignment(Alignment::SpaceBetween)
            .constraints([Constraint::Fixed(10)]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // Single item: starts at 0, no extra spacing
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[0].width, 10);
    }

    #[test]
    fn invariant_rects_within_bounds() {
        let area = Rect::new(10, 20, 80, 60);
        let flex = Flex::horizontal()
            .margin(Sides::all(5))
            .gap(2)
            .constraints([
                Constraint::Fixed(15),
                Constraint::Percentage(30.0),
                Constraint::Min(10),
            ]);
        let rects = flex.split(area);

        // All rects should be within the inner area (after margin)
        let inner = area.inner(Sides::all(5));
        for rect in &rects {
            assert!(
                rect.x >= inner.x && rect.right() <= inner.right(),
                "Rect {:?} exceeds horizontal bounds of {:?}",
                rect,
                inner
            );
            assert!(
                rect.y >= inner.y && rect.bottom() <= inner.bottom(),
                "Rect {:?} exceeds vertical bounds of {:?}",
                rect,
                inner
            );
        }
    }

    // --- Fill constraint ---

    #[test]
    fn fill_takes_remaining_space() {
        let flex = Flex::horizontal().constraints([Constraint::Fixed(20), Constraint::Fill]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects[0].width, 20);
        assert_eq!(rects[1].width, 80); // Fill gets remaining
    }

    #[test]
    fn multiple_fills_share_space() {
        let flex = Flex::horizontal().constraints([Constraint::Fill, Constraint::Fill]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects[0].width, 50);
        assert_eq!(rects[1].width, 50);
    }

    // --- FitContent constraint ---

    #[test]
    fn fit_content_uses_preferred_size() {
        let flex = Flex::horizontal().constraints([Constraint::FitContent, Constraint::Fill]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |idx, _| {
            if idx == 0 {
                LayoutSizeHint {
                    min: 5,
                    preferred: 30,
                    max: None,
                }
            } else {
                LayoutSizeHint::ZERO
            }
        });
        assert_eq!(rects[0].width, 30); // FitContent gets preferred
        assert_eq!(rects[1].width, 70); // Fill gets remainder
    }

    #[test]
    fn fit_content_clamps_to_available() {
        let flex = Flex::horizontal().constraints([Constraint::FitContent, Constraint::FitContent]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |_, _| LayoutSizeHint {
            min: 10,
            preferred: 80,
            max: None,
        });
        // First FitContent takes 80, second gets remaining 20
        assert_eq!(rects[0].width, 80);
        assert_eq!(rects[1].width, 20);
    }

    #[test]
    fn fit_content_without_measurer_gets_zero() {
        // Without measurer (via split()), FitContent gets zero from default hint
        let flex = Flex::horizontal().constraints([Constraint::FitContent, Constraint::Fill]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        assert_eq!(rects[0].width, 0); // No preferred size
        assert_eq!(rects[1].width, 100); // Fill gets all
    }

    // --- FitContentBounded constraint ---

    #[test]
    fn fit_content_bounded_clamps_to_min() {
        let flex = Flex::horizontal().constraints([
            Constraint::FitContentBounded { min: 20, max: 50 },
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |_, _| LayoutSizeHint {
            min: 5,
            preferred: 10, // Below min bound
            max: None,
        });
        assert_eq!(rects[0].width, 20); // Clamped to min bound
        assert_eq!(rects[1].width, 80);
    }

    #[test]
    fn fit_content_bounded_clamps_to_max() {
        let flex = Flex::horizontal().constraints([
            Constraint::FitContentBounded { min: 10, max: 30 },
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |_, _| LayoutSizeHint {
            min: 5,
            preferred: 50, // Above max bound
            max: None,
        });
        assert_eq!(rects[0].width, 30); // Clamped to max bound
        assert_eq!(rects[1].width, 70);
    }

    #[test]
    fn fit_content_bounded_uses_preferred_when_in_range() {
        let flex = Flex::horizontal().constraints([
            Constraint::FitContentBounded { min: 10, max: 50 },
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |_, _| LayoutSizeHint {
            min: 5,
            preferred: 35, // Within bounds
            max: None,
        });
        assert_eq!(rects[0].width, 35);
        assert_eq!(rects[1].width, 65);
    }

    // --- FitMin constraint ---

    #[test]
    fn fit_min_uses_minimum_size() {
        let flex = Flex::horizontal().constraints([Constraint::FitMin, Constraint::Fill]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |idx, _| {
            if idx == 0 {
                LayoutSizeHint {
                    min: 15,
                    preferred: 40,
                    max: None,
                }
            } else {
                LayoutSizeHint::ZERO
            }
        });
        // FitMin gets minimum (15) + grows with remaining
        // Since Fill is also a grow candidate, they share the 85 remaining
        // FitMin base: 15, grows by (85/2) = 42.5 rounded to 42
        // Actually: FitMin gets 15 initially, remaining = 85
        // Then both FitMin and Fill compete for 85 with equal weight
        // FitMin gets 15 + 42 = 57, Fill gets 43
        // Wait, let me trace through the logic more carefully.
        //
        // After first pass: FitMin gets 15, remaining = 85. FitMin added to grow_indices.
        // Fill gets 0, added to grow_indices.
        // In grow loop: 85 distributed evenly (weight 1 each) = 42.5 each
        // FitMin: 15 + 42 = 57 (or 58 if rounding gives it the extra)
        // Actually the last item gets remainder to ensure exact sum
        let total: u16 = rects.iter().map(|r| r.width).sum();
        assert_eq!(total, 100);
        assert!(rects[0].width >= 15, "FitMin should get at least minimum");
    }

    #[test]
    fn fit_min_without_measurer_gets_zero() {
        let flex = Flex::horizontal().constraints([Constraint::FitMin, Constraint::Fill]);
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        // Without measurer, min is 0, so FitMin gets 0 initially, then grows
        // Both FitMin and Fill share 100 evenly
        assert_eq!(rects[0].width, 50);
        assert_eq!(rects[1].width, 50);
    }

    // --- LayoutSizeHint tests ---

    #[test]
    fn layout_size_hint_zero_is_default() {
        assert_eq!(LayoutSizeHint::default(), LayoutSizeHint::ZERO);
    }

    #[test]
    fn layout_size_hint_exact() {
        let h = LayoutSizeHint::exact(25);
        assert_eq!(h.min, 25);
        assert_eq!(h.preferred, 25);
        assert_eq!(h.max, Some(25));
    }

    #[test]
    fn layout_size_hint_at_least() {
        let h = LayoutSizeHint::at_least(10, 30);
        assert_eq!(h.min, 10);
        assert_eq!(h.preferred, 30);
        assert_eq!(h.max, None);
    }

    #[test]
    fn layout_size_hint_clamp() {
        let h = LayoutSizeHint {
            min: 10,
            preferred: 20,
            max: Some(30),
        };
        assert_eq!(h.clamp(5), 10); // Below min
        assert_eq!(h.clamp(15), 15); // In range
        assert_eq!(h.clamp(50), 30); // Above max
    }

    #[test]
    fn layout_size_hint_clamp_unbounded() {
        let h = LayoutSizeHint::at_least(5, 10);
        assert_eq!(h.clamp(3), 5); // Below min
        assert_eq!(h.clamp(1000), 1000); // No max, stays as-is
    }

    // --- Integration: FitContent with other constraints ---

    #[test]
    fn fit_content_with_fixed_and_fill() {
        let flex = Flex::horizontal().constraints([
            Constraint::Fixed(20),
            Constraint::FitContent,
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 100, 10), |idx, _| {
            if idx == 1 {
                LayoutSizeHint {
                    min: 5,
                    preferred: 25,
                    max: None,
                }
            } else {
                LayoutSizeHint::ZERO
            }
        });
        assert_eq!(rects[0].width, 20); // Fixed
        assert_eq!(rects[1].width, 25); // FitContent preferred
        assert_eq!(rects[2].width, 55); // Fill gets remainder
    }

    #[test]
    fn total_allocation_never_exceeds_available_with_fit_content() {
        for available in [10u16, 50, 100, 255] {
            let flex = Flex::horizontal().constraints([
                Constraint::FitContent,
                Constraint::FitContent,
                Constraint::Fill,
            ]);
            let rects =
                flex.split_with_measurer(Rect::new(0, 0, available, 10), |_, _| LayoutSizeHint {
                    min: 10,
                    preferred: 40,
                    max: None,
                });
            let total: u16 = rects.iter().map(|r| r.width).sum();
            assert!(
                total <= available,
                "Total {} exceeded available {} with FitContent",
                total,
                available
            );
        }
    }

    // -----------------------------------------------------------------------
    // Stable Layout Rounding Tests (bd-4kq0.4.1)
    // -----------------------------------------------------------------------

    mod rounding_tests {
        use super::super::*;

        // --- Sum conservation (REQUIRED) ---

        #[test]
        fn rounding_conserves_sum_exact() {
            let result = round_layout_stable(&[10.0, 20.0, 10.0], 40, None);
            assert_eq!(result.iter().copied().sum::<u16>(), 40);
            assert_eq!(result, vec![10, 20, 10]);
        }

        #[test]
        fn rounding_conserves_sum_fractional() {
            let result = round_layout_stable(&[10.4, 20.6, 9.0], 40, None);
            assert_eq!(
                result.iter().copied().sum::<u16>(),
                40,
                "Sum must equal total: {:?}",
                result
            );
        }

        #[test]
        fn rounding_conserves_sum_many_fractions() {
            let targets = vec![20.2, 20.2, 20.2, 20.2, 19.2];
            let result = round_layout_stable(&targets, 100, None);
            assert_eq!(
                result.iter().copied().sum::<u16>(),
                100,
                "Sum must be exactly 100: {:?}",
                result
            );
        }

        #[test]
        fn rounding_conserves_sum_all_half() {
            let targets = vec![10.5, 10.5, 10.5, 10.5];
            let result = round_layout_stable(&targets, 42, None);
            assert_eq!(
                result.iter().copied().sum::<u16>(),
                42,
                "Sum must be exactly 42: {:?}",
                result
            );
        }

        // --- Bounded displacement ---

        #[test]
        fn rounding_displacement_bounded() {
            let targets = vec![33.33, 33.33, 33.34];
            let result = round_layout_stable(&targets, 100, None);
            assert_eq!(result.iter().copied().sum::<u16>(), 100);

            for (i, (&x, &r)) in result.iter().zip(targets.iter()).enumerate() {
                let floor = r.floor() as u16;
                let ceil = floor + 1;
                assert!(
                    x == floor || x == ceil,
                    "Element {} = {} not in {{floor={}, ceil={}}} of target {}",
                    i,
                    x,
                    floor,
                    ceil,
                    r
                );
            }
        }

        // --- Temporal tie-break (REQUIRED) ---

        #[test]
        fn temporal_tiebreak_stable_when_unchanged() {
            let targets = vec![10.5, 10.5, 10.5, 10.5];
            let first = round_layout_stable(&targets, 42, None);
            let second = round_layout_stable(&targets, 42, Some(first.clone()));
            assert_eq!(
                first, second,
                "Identical targets should produce identical results"
            );
        }

        #[test]
        fn temporal_tiebreak_prefers_previous_direction() {
            let targets = vec![10.5, 10.5];
            let total = 21;
            let first = round_layout_stable(&targets, total, None);
            assert_eq!(first.iter().copied().sum::<u16>(), total);
            let second = round_layout_stable(&targets, total, Some(first.clone()));
            assert_eq!(first, second, "Should maintain rounding direction");
        }

        #[test]
        fn temporal_tiebreak_adapts_to_changed_targets() {
            let targets_a = vec![10.5, 10.5];
            let result_a = round_layout_stable(&targets_a, 21, None);
            let targets_b = vec![15.7, 5.3];
            let result_b = round_layout_stable(&targets_b, 21, Some(result_a));
            assert_eq!(result_b.iter().copied().sum::<u16>(), 21);
            assert!(result_b[0] > result_b[1], "Should follow larger target");
        }

        // --- Property: min displacement (REQUIRED) ---

        #[test]
        fn property_min_displacement_brute_force_small() {
            let targets = vec![3.3, 3.3, 3.4];
            let total: u16 = 10;
            let result = round_layout_stable(&targets, total, None);
            let our_displacement: f64 = result
                .iter()
                .zip(targets.iter())
                .map(|(&x, &r)| (x as f64 - r).abs())
                .sum();

            let mut min_displacement = f64::MAX;
            let floors: Vec<u16> = targets.iter().map(|&r| r.floor() as u16).collect();
            let ceils: Vec<u16> = targets.iter().map(|&r| r.floor() as u16 + 1).collect();

            for a in floors[0]..=ceils[0] {
                for b in floors[1]..=ceils[1] {
                    for c in floors[2]..=ceils[2] {
                        if a + b + c == total {
                            let disp = (a as f64 - targets[0]).abs()
                                + (b as f64 - targets[1]).abs()
                                + (c as f64 - targets[2]).abs();
                            if disp < min_displacement {
                                min_displacement = disp;
                            }
                        }
                    }
                }
            }

            assert!(
                (our_displacement - min_displacement).abs() < 1e-10,
                "Our displacement {} should match optimal {}: {:?}",
                our_displacement,
                min_displacement,
                result
            );
        }

        // --- Determinism ---

        #[test]
        fn rounding_deterministic() {
            let targets = vec![7.7, 8.3, 14.0];
            let a = round_layout_stable(&targets, 30, None);
            let b = round_layout_stable(&targets, 30, None);
            assert_eq!(a, b, "Same inputs must produce identical outputs");
        }

        // --- Edge cases ---

        #[test]
        fn rounding_empty_targets() {
            let result = round_layout_stable(&[], 0, None);
            assert!(result.is_empty());
        }

        #[test]
        fn rounding_single_element() {
            let result = round_layout_stable(&[10.7], 11, None);
            assert_eq!(result, vec![11]);
        }

        #[test]
        fn rounding_zero_total() {
            let result = round_layout_stable(&[5.0, 5.0], 0, None);
            assert_eq!(result.iter().copied().sum::<u16>(), 0);
        }

        #[test]
        fn rounding_all_zeros() {
            let result = round_layout_stable(&[0.0, 0.0, 0.0], 0, None);
            assert_eq!(result, vec![0, 0, 0]);
        }

        #[test]
        fn rounding_integer_targets() {
            let result = round_layout_stable(&[10.0, 20.0, 30.0], 60, None);
            assert_eq!(result, vec![10, 20, 30]);
        }

        #[test]
        fn rounding_large_deficit() {
            let result = round_layout_stable(&[0.9, 0.9, 0.9], 3, None);
            assert_eq!(result.iter().copied().sum::<u16>(), 3);
            assert_eq!(result, vec![1, 1, 1]);
        }

        #[test]
        fn rounding_with_prev_different_length() {
            let result = round_layout_stable(&[10.5, 10.5], 21, Some(vec![11, 10, 5]));
            assert_eq!(result.iter().copied().sum::<u16>(), 21);
        }

        #[test]
        fn rounding_very_small_fractions() {
            let targets = vec![10.001, 20.001, 9.998];
            let result = round_layout_stable(&targets, 40, None);
            assert_eq!(result.iter().copied().sum::<u16>(), 40);
        }

        #[test]
        fn rounding_conserves_sum_stress() {
            let n = 50;
            let targets: Vec<f64> = (0..n).map(|i| 2.0 + (i as f64 * 0.037)).collect();
            let total = 120u16;
            let result = round_layout_stable(&targets, total, None);
            assert_eq!(
                result.iter().copied().sum::<u16>(),
                total,
                "Sum must be exactly {} for {} items: {:?}",
                total,
                n,
                result
            );
        }
    }

    // -----------------------------------------------------------------------
    // Property Tests: Constraint Satisfaction (bd-4kq0.4.3)
    // -----------------------------------------------------------------------

    mod property_constraint_tests {
        use super::super::*;

        /// Deterministic LCG pseudo-random number generator (no external deps).
        struct Lcg(u64);

        impl Lcg {
            fn new(seed: u64) -> Self {
                Self(seed)
            }
            fn next_u32(&mut self) -> u32 {
                self.0 = self
                    .0
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (self.0 >> 33) as u32
            }
            fn next_u16_range(&mut self, lo: u16, hi: u16) -> u16 {
                if lo >= hi {
                    return lo;
                }
                lo + (self.next_u32() % (hi - lo) as u32) as u16
            }
            fn next_f32(&mut self) -> f32 {
                (self.next_u32() & 0x00FF_FFFF) as f32 / 16_777_216.0
            }
        }

        /// Generate a random constraint from the LCG.
        fn random_constraint(rng: &mut Lcg) -> Constraint {
            match rng.next_u32() % 7 {
                0 => Constraint::Fixed(rng.next_u16_range(1, 80)),
                1 => Constraint::Percentage(rng.next_f32() * 100.0),
                2 => Constraint::Min(rng.next_u16_range(0, 40)),
                3 => Constraint::Max(rng.next_u16_range(5, 120)),
                4 => {
                    let n = rng.next_u32() % 5 + 1;
                    let d = rng.next_u32() % 5 + 1;
                    Constraint::Ratio(n, d)
                }
                5 => Constraint::Fill,
                _ => Constraint::FitContent,
            }
        }

        #[test]
        fn property_constraints_respected_fixed() {
            let mut rng = Lcg::new(0xDEAD_BEEF);
            for _ in 0..200 {
                let fixed_val = rng.next_u16_range(1, 60);
                let avail = rng.next_u16_range(10, 200);
                let flex = Flex::horizontal().constraints([Constraint::Fixed(fixed_val)]);
                let rects = flex.split(Rect::new(0, 0, avail, 10));
                assert!(
                    rects[0].width <= fixed_val.min(avail),
                    "Fixed({}) in avail {} -> width {}",
                    fixed_val,
                    avail,
                    rects[0].width
                );
            }
        }

        #[test]
        fn property_constraints_respected_max() {
            let mut rng = Lcg::new(0xCAFE_BABE);
            for _ in 0..200 {
                let max_val = rng.next_u16_range(5, 80);
                let avail = rng.next_u16_range(10, 200);
                let flex =
                    Flex::horizontal().constraints([Constraint::Max(max_val), Constraint::Fill]);
                let rects = flex.split(Rect::new(0, 0, avail, 10));
                assert!(
                    rects[0].width <= max_val,
                    "Max({}) in avail {} -> width {}",
                    max_val,
                    avail,
                    rects[0].width
                );
            }
        }

        #[test]
        fn property_constraints_respected_min() {
            let mut rng = Lcg::new(0xBAAD_F00D);
            for _ in 0..200 {
                let min_val = rng.next_u16_range(0, 40);
                let avail = rng.next_u16_range(min_val.max(1), 200);
                let flex = Flex::horizontal().constraints([Constraint::Min(min_val)]);
                let rects = flex.split(Rect::new(0, 0, avail, 10));
                assert!(
                    rects[0].width >= min_val,
                    "Min({}) in avail {} -> width {}",
                    min_val,
                    avail,
                    rects[0].width
                );
            }
        }

        #[test]
        fn property_constraints_respected_ratio_proportional() {
            let mut rng = Lcg::new(0x1234_5678);
            for _ in 0..200 {
                let n1 = rng.next_u32() % 5 + 1;
                let n2 = rng.next_u32() % 5 + 1;
                let d = rng.next_u32() % 5 + 1;
                let avail = rng.next_u16_range(20, 200);
                let flex = Flex::horizontal()
                    .constraints([Constraint::Ratio(n1, d), Constraint::Ratio(n2, d)]);
                let rects = flex.split(Rect::new(0, 0, avail, 10));
                let w1 = rects[0].width as f64;
                let w2 = rects[1].width as f64;
                let total = w1 + w2;
                if total > 0.0 {
                    let expected_ratio = n1 as f64 / (n1 + n2) as f64;
                    let actual_ratio = w1 / total;
                    assert!(
                        (actual_ratio - expected_ratio).abs() < 0.15 || total < 4.0,
                        "Ratio({},{})/({}+{}) avail={}: ~{:.2} got {:.2} (w1={}, w2={})",
                        n1,
                        d,
                        n1,
                        n2,
                        avail,
                        expected_ratio,
                        actual_ratio,
                        w1,
                        w2
                    );
                }
            }
        }

        #[test]
        fn property_total_allocation_never_exceeds_available() {
            let mut rng = Lcg::new(0xFACE_FEED);
            for _ in 0..500 {
                let n = (rng.next_u32() % 6 + 1) as usize;
                let constraints: Vec<Constraint> =
                    (0..n).map(|_| random_constraint(&mut rng)).collect();
                let avail = rng.next_u16_range(5, 200);
                let dir = if rng.next_u32().is_multiple_of(2) {
                    Direction::Horizontal
                } else {
                    Direction::Vertical
                };
                let flex = Flex::default().direction(dir).constraints(constraints);
                let area = Rect::new(0, 0, avail, avail);
                let rects = flex.split(area);
                let total: u16 = rects
                    .iter()
                    .map(|r| match dir {
                        Direction::Horizontal => r.width,
                        Direction::Vertical => r.height,
                    })
                    .sum();
                assert!(
                    total <= avail,
                    "Total {} exceeded available {} with {} constraints",
                    total,
                    avail,
                    n
                );
            }
        }

        #[test]
        fn property_no_overlap_horizontal() {
            let mut rng = Lcg::new(0xABCD_1234);
            for _ in 0..300 {
                let n = (rng.next_u32() % 5 + 2) as usize;
                let constraints: Vec<Constraint> =
                    (0..n).map(|_| random_constraint(&mut rng)).collect();
                let avail = rng.next_u16_range(20, 200);
                let flex = Flex::horizontal().constraints(constraints);
                let rects = flex.split(Rect::new(0, 0, avail, 10));

                for i in 1..rects.len() {
                    let prev_end = rects[i - 1].x + rects[i - 1].width;
                    assert!(
                        rects[i].x >= prev_end,
                        "Overlap at {}: prev ends {}, next starts {}",
                        i,
                        prev_end,
                        rects[i].x
                    );
                }
            }
        }

        #[test]
        fn property_deterministic_across_runs() {
            let mut rng = Lcg::new(0x9999_8888);
            for _ in 0..100 {
                let n = (rng.next_u32() % 5 + 1) as usize;
                let constraints: Vec<Constraint> =
                    (0..n).map(|_| random_constraint(&mut rng)).collect();
                let avail = rng.next_u16_range(10, 200);
                let r1 = Flex::horizontal()
                    .constraints(constraints.clone())
                    .split(Rect::new(0, 0, avail, 10));
                let r2 = Flex::horizontal()
                    .constraints(constraints)
                    .split(Rect::new(0, 0, avail, 10));
                assert_eq!(r1, r2, "Determinism violation at avail={}", avail);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property Tests: Temporal Stability (bd-4kq0.4.3)
    // -----------------------------------------------------------------------

    mod property_temporal_tests {
        use super::super::*;
        use crate::cache::{CoherenceCache, CoherenceId};

        /// Deterministic LCG.
        struct Lcg(u64);

        impl Lcg {
            fn new(seed: u64) -> Self {
                Self(seed)
            }
            fn next_u32(&mut self) -> u32 {
                self.0 = self
                    .0
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (self.0 >> 33) as u32
            }
        }

        #[test]
        fn property_temporal_stability_small_resize() {
            let constraints = [
                Constraint::Percentage(33.3),
                Constraint::Percentage(33.3),
                Constraint::Fill,
            ];
            let mut coherence = CoherenceCache::new(64);
            let id = CoherenceId::new(&constraints, Direction::Horizontal);

            for total in [80u16, 100, 120] {
                let flex = Flex::horizontal().constraints(constraints);
                let rects = flex.split(Rect::new(0, 0, total, 10));
                let widths: Vec<u16> = rects.iter().map(|r| r.width).collect();

                let targets: Vec<f64> = widths.iter().map(|&w| w as f64).collect();
                let prev = coherence.get(&id);
                let rounded = round_layout_stable(&targets, total, prev);

                if let Some(old) = coherence.get(&id) {
                    let (sum_disp, max_disp) = coherence.displacement(&id, &rounded);
                    assert!(
                        max_disp <= total.abs_diff(old.iter().copied().sum()) as u32 + 1,
                        "max_disp={} too large for size change {} -> {}",
                        max_disp,
                        old.iter().copied().sum::<u16>(),
                        total
                    );
                    let _ = sum_disp;
                }
                coherence.store(id, rounded);
            }
        }

        #[test]
        fn property_temporal_stability_random_walk() {
            let constraints = [
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ];
            let id = CoherenceId::new(&constraints, Direction::Horizontal);
            let mut coherence = CoherenceCache::new(64);
            let mut rng = Lcg::new(0x5555_AAAA);
            let mut total: u16 = 90;

            for step in 0..200 {
                let prev_total = total;
                let delta = (rng.next_u32() % 7) as i32 - 3;
                total = (total as i32 + delta).clamp(10, 250) as u16;

                let flex = Flex::horizontal().constraints(constraints);
                let rects = flex.split(Rect::new(0, 0, total, 10));
                let widths: Vec<u16> = rects.iter().map(|r| r.width).collect();

                let targets: Vec<f64> = widths.iter().map(|&w| w as f64).collect();
                let prev = coherence.get(&id);
                let rounded = round_layout_stable(&targets, total, prev);

                if coherence.get(&id).is_some() {
                    let (_, max_disp) = coherence.displacement(&id, &rounded);
                    let size_change = total.abs_diff(prev_total);
                    assert!(
                        max_disp <= size_change as u32 + 2,
                        "step {}: max_disp={} exceeds size_change={} + 2",
                        step,
                        max_disp,
                        size_change
                    );
                }
                coherence.store(id, rounded);
            }
        }

        #[test]
        fn property_temporal_stability_identical_frames() {
            let constraints = [
                Constraint::Fixed(20),
                Constraint::Fill,
                Constraint::Fixed(15),
            ];
            let id = CoherenceId::new(&constraints, Direction::Horizontal);
            let mut coherence = CoherenceCache::new(64);

            let flex = Flex::horizontal().constraints(constraints);
            let rects = flex.split(Rect::new(0, 0, 100, 10));
            let widths: Vec<u16> = rects.iter().map(|r| r.width).collect();
            coherence.store(id, widths.clone());

            for _ in 0..10 {
                let targets: Vec<f64> = widths.iter().map(|&w| w as f64).collect();
                let prev = coherence.get(&id);
                let rounded = round_layout_stable(&targets, 100, prev);
                let (sum_disp, max_disp) = coherence.displacement(&id, &rounded);
                assert_eq!(sum_disp, 0, "Identical frames: zero displacement");
                assert_eq!(max_disp, 0);
                coherence.store(id, rounded);
            }
        }

        #[test]
        fn property_temporal_coherence_sweep() {
            let constraints = [
                Constraint::Percentage(25.0),
                Constraint::Percentage(50.0),
                Constraint::Fill,
            ];
            let id = CoherenceId::new(&constraints, Direction::Horizontal);
            let mut coherence = CoherenceCache::new(64);
            let mut total_displacement: u64 = 0;

            for total in 60u16..=140 {
                let flex = Flex::horizontal().constraints(constraints);
                let rects = flex.split(Rect::new(0, 0, total, 10));
                let widths: Vec<u16> = rects.iter().map(|r| r.width).collect();

                let targets: Vec<f64> = widths.iter().map(|&w| w as f64).collect();
                let prev = coherence.get(&id);
                let rounded = round_layout_stable(&targets, total, prev);

                if coherence.get(&id).is_some() {
                    let (sum_disp, _) = coherence.displacement(&id, &rounded);
                    total_displacement += sum_disp;
                }
                coherence.store(id, rounded);
            }

            assert!(
                total_displacement <= 80 * 3,
                "Total displacement {} exceeds bound for 80-step sweep",
                total_displacement
            );
        }
    }

    // -----------------------------------------------------------------------
    // Snapshot Regression: Canonical Flex/Grid Layouts (bd-4kq0.4.3)
    // -----------------------------------------------------------------------

    mod snapshot_layout_tests {
        use super::super::*;
        use crate::grid::{Grid, GridArea};

        fn snapshot_flex(
            constraints: &[Constraint],
            dir: Direction,
            width: u16,
            height: u16,
        ) -> String {
            let flex = Flex::default()
                .direction(dir)
                .constraints(constraints.iter().copied());
            let rects = flex.split(Rect::new(0, 0, width, height));
            let mut out = format!(
                "Flex {:?} {}x{} ({} constraints)\n",
                dir,
                width,
                height,
                constraints.len()
            );
            for (i, r) in rects.iter().enumerate() {
                out.push_str(&format!(
                    "  [{}] x={} y={} w={} h={}\n",
                    i, r.x, r.y, r.width, r.height
                ));
            }
            let total: u16 = rects
                .iter()
                .map(|r| match dir {
                    Direction::Horizontal => r.width,
                    Direction::Vertical => r.height,
                })
                .sum();
            out.push_str(&format!("  total={}\n", total));
            out
        }

        fn snapshot_grid(
            rows: &[Constraint],
            cols: &[Constraint],
            areas: &[(&str, GridArea)],
            width: u16,
            height: u16,
        ) -> String {
            let mut grid = Grid::new()
                .rows(rows.iter().copied())
                .columns(cols.iter().copied());
            for &(name, area) in areas {
                grid = grid.area(name, area);
            }
            let layout = grid.split(Rect::new(0, 0, width, height));

            let mut out = format!(
                "Grid {}x{} ({}r x {}c)\n",
                width,
                height,
                rows.len(),
                cols.len()
            );
            for r in 0..rows.len() {
                for c in 0..cols.len() {
                    let rect = layout.cell(r, c);
                    out.push_str(&format!(
                        "  [{},{}] x={} y={} w={} h={}\n",
                        r, c, rect.x, rect.y, rect.width, rect.height
                    ));
                }
            }
            for &(name, _) in areas {
                if let Some(rect) = layout.area(name) {
                    out.push_str(&format!(
                        "  area({}) x={} y={} w={} h={}\n",
                        name, rect.x, rect.y, rect.width, rect.height
                    ));
                }
            }
            out
        }

        // --- Flex snapshots: 80x24 ---

        #[test]
        fn snapshot_flex_thirds_80x24() {
            let snap = snapshot_flex(
                &[
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                ],
                Direction::Horizontal,
                80,
                24,
            );
            assert_eq!(
                snap,
                "\
Flex Horizontal 80x24 (3 constraints)
  [0] x=0 y=0 w=26 h=24
  [1] x=26 y=0 w=26 h=24
  [2] x=52 y=0 w=28 h=24
  total=80
"
            );
        }

        #[test]
        fn snapshot_flex_sidebar_content_80x24() {
            let snap = snapshot_flex(
                &[Constraint::Fixed(20), Constraint::Fill],
                Direction::Horizontal,
                80,
                24,
            );
            assert_eq!(
                snap,
                "\
Flex Horizontal 80x24 (2 constraints)
  [0] x=0 y=0 w=20 h=24
  [1] x=20 y=0 w=60 h=24
  total=80
"
            );
        }

        #[test]
        fn snapshot_flex_header_body_footer_80x24() {
            let snap = snapshot_flex(
                &[Constraint::Fixed(3), Constraint::Fill, Constraint::Fixed(1)],
                Direction::Vertical,
                80,
                24,
            );
            assert_eq!(
                snap,
                "\
Flex Vertical 80x24 (3 constraints)
  [0] x=0 y=0 w=80 h=3
  [1] x=0 y=3 w=80 h=20
  [2] x=0 y=23 w=80 h=1
  total=24
"
            );
        }

        // --- Flex snapshots: 120x40 ---

        #[test]
        fn snapshot_flex_thirds_120x40() {
            let snap = snapshot_flex(
                &[
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                ],
                Direction::Horizontal,
                120,
                40,
            );
            assert_eq!(
                snap,
                "\
Flex Horizontal 120x40 (3 constraints)
  [0] x=0 y=0 w=40 h=40
  [1] x=40 y=0 w=40 h=40
  [2] x=80 y=0 w=40 h=40
  total=120
"
            );
        }

        #[test]
        fn snapshot_flex_sidebar_content_120x40() {
            let snap = snapshot_flex(
                &[Constraint::Fixed(20), Constraint::Fill],
                Direction::Horizontal,
                120,
                40,
            );
            assert_eq!(
                snap,
                "\
Flex Horizontal 120x40 (2 constraints)
  [0] x=0 y=0 w=20 h=40
  [1] x=20 y=0 w=100 h=40
  total=120
"
            );
        }

        #[test]
        fn snapshot_flex_percentage_mix_120x40() {
            let snap = snapshot_flex(
                &[
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(50.0),
                    Constraint::Fill,
                ],
                Direction::Horizontal,
                120,
                40,
            );
            assert_eq!(
                snap,
                "\
Flex Horizontal 120x40 (3 constraints)
  [0] x=0 y=0 w=30 h=40
  [1] x=30 y=0 w=60 h=40
  [2] x=90 y=0 w=30 h=40
  total=120
"
            );
        }

        // --- Grid snapshots: 80x24 ---

        #[test]
        fn snapshot_grid_2x2_80x24() {
            let snap = snapshot_grid(
                &[Constraint::Fixed(3), Constraint::Fill],
                &[Constraint::Fixed(20), Constraint::Fill],
                &[
                    ("header", GridArea::span(0, 0, 1, 2)),
                    ("sidebar", GridArea::span(1, 0, 1, 1)),
                    ("content", GridArea::cell(1, 1)),
                ],
                80,
                24,
            );
            assert_eq!(
                snap,
                "\
Grid 80x24 (2r x 2c)
  [0,0] x=0 y=0 w=20 h=3
  [0,1] x=20 y=0 w=60 h=3
  [1,0] x=0 y=3 w=20 h=21
  [1,1] x=20 y=3 w=60 h=21
  area(header) x=0 y=0 w=80 h=3
  area(sidebar) x=0 y=3 w=20 h=21
  area(content) x=20 y=3 w=60 h=21
"
            );
        }

        #[test]
        fn snapshot_grid_3x3_80x24() {
            let snap = snapshot_grid(
                &[Constraint::Fixed(1), Constraint::Fill, Constraint::Fixed(1)],
                &[
                    Constraint::Fixed(10),
                    Constraint::Fill,
                    Constraint::Fixed(10),
                ],
                &[],
                80,
                24,
            );
            assert_eq!(
                snap,
                "\
Grid 80x24 (3r x 3c)
  [0,0] x=0 y=0 w=10 h=1
  [0,1] x=10 y=0 w=60 h=1
  [0,2] x=70 y=0 w=10 h=1
  [1,0] x=0 y=1 w=10 h=22
  [1,1] x=10 y=1 w=60 h=22
  [1,2] x=70 y=1 w=10 h=22
  [2,0] x=0 y=23 w=10 h=1
  [2,1] x=10 y=23 w=60 h=1
  [2,2] x=70 y=23 w=10 h=1
"
            );
        }

        // --- Grid snapshots: 120x40 ---

        #[test]
        fn snapshot_grid_2x2_120x40() {
            let snap = snapshot_grid(
                &[Constraint::Fixed(3), Constraint::Fill],
                &[Constraint::Fixed(20), Constraint::Fill],
                &[
                    ("header", GridArea::span(0, 0, 1, 2)),
                    ("sidebar", GridArea::span(1, 0, 1, 1)),
                    ("content", GridArea::cell(1, 1)),
                ],
                120,
                40,
            );
            assert_eq!(
                snap,
                "\
Grid 120x40 (2r x 2c)
  [0,0] x=0 y=0 w=20 h=3
  [0,1] x=20 y=0 w=100 h=3
  [1,0] x=0 y=3 w=20 h=37
  [1,1] x=20 y=3 w=100 h=37
  area(header) x=0 y=0 w=120 h=3
  area(sidebar) x=0 y=3 w=20 h=37
  area(content) x=20 y=3 w=100 h=37
"
            );
        }

        #[test]
        fn snapshot_grid_dashboard_120x40() {
            let snap = snapshot_grid(
                &[
                    Constraint::Fixed(3),
                    Constraint::Percentage(60.0),
                    Constraint::Fill,
                ],
                &[Constraint::Percentage(30.0), Constraint::Fill],
                &[
                    ("nav", GridArea::span(0, 0, 1, 2)),
                    ("chart", GridArea::cell(1, 0)),
                    ("detail", GridArea::cell(1, 1)),
                    ("log", GridArea::span(2, 0, 1, 2)),
                ],
                120,
                40,
            );
            assert_eq!(
                snap,
                "\
Grid 120x40 (3r x 2c)
  [0,0] x=0 y=0 w=36 h=3
  [0,1] x=36 y=0 w=84 h=3
  [1,0] x=0 y=3 w=36 h=24
  [1,1] x=36 y=3 w=84 h=24
  [2,0] x=0 y=27 w=36 h=13
  [2,1] x=36 y=27 w=84 h=13
  area(nav) x=0 y=0 w=120 h=3
  area(chart) x=0 y=3 w=36 h=24
  area(detail) x=36 y=3 w=84 h=24
  area(log) x=0 y=27 w=120 h=13
"
            );
        }
    }
}
