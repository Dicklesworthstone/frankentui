#![forbid(unsafe_code)]

//! Layout primitives and solvers.
//!
//! This crate provides layout components for terminal UIs:
//!
//! - [`Flex`] - 1D constraint-based layout (rows or columns)
//! - [`Grid`] - 2D constraint-based layout with cell spanning
//! - [`Constraint`] - Size constraints (Fixed, Percentage, Min, Max, Ratio)
//! - [`debug`] - Layout constraint debugging and introspection

pub mod debug;
pub mod grid;

pub use ftui_core::geometry::{Rect, Sides};
pub use grid::{Grid, GridArea, GridLayout};
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
}

/// The direction to layout items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
                        current_pos = current_pos.saturating_add(unit * 2);
                    }
                }
                _ => {}
            }
        }

        rects
    }
}

/// Solve 1D constraints to determine sizes.
///
/// This shared logic is used by both Flex and Grid layouts.
pub(crate) fn solve_constraints(constraints: &[Constraint], available_size: u16) -> Vec<u16> {
    let mut sizes = vec![0u16; constraints.len()];
    let mut remaining = available_size;
    let mut grow_indices = Vec::new();

    // 1. Allocate Fixed, Percentage, Min
    for (i, &constraint) in constraints.iter().enumerate() {
        match constraint {
            Constraint::Fixed(size) => {
                let size = min(size, remaining);
                sizes[i] = size;
                remaining -= size;
            }
            Constraint::Percentage(p) => {
                let size = (available_size as f32 * p / 100.0).round() as u16;
                let size = min(size, remaining);
                sizes[i] = size;
                remaining -= size;
            }
            Constraint::Min(min_size) => {
                let size = min(min_size, remaining);
                sizes[i] = size;
                remaining -= size;
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
                && sizes[i] + shares[i] > max_val
            {
                violations.push(i);
            }
        }

        if violations.is_empty() {
            // No violations, commit shares and exit
            for &i in &grow_indices {
                sizes[i] += shares[i];
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
}
