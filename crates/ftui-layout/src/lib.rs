#![forbid(unsafe_code)]

//! Layout primitives and solvers.

pub use ftui_core::geometry::{Rect, Sides};
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
    #[default]
    Start,
    Center,
    End,
    SpaceAround,
    SpaceBetween,
}

/// Size negotiation hints for layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct Measurement {
    pub min_width: u16,
    pub min_height: u16,
    pub max_width: Option<u16>,
    pub max_height: Option<u16>,
}

impl Measurement {
    pub fn fixed(width: u16, height: u16) -> Self {
        Self {
            min_width: width,
            min_height: height,
            max_width: Some(width),
            max_height: Some(height),
        }
    }

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
        let sizes = self.solve_constraints(available_size);

        // Convert sizes to rects
        self.sizes_to_rects(inner, &sizes)
    }

    fn solve_constraints(&self, available_size: u16) -> Vec<u16> {
        let mut sizes = vec![0u16; self.constraints.len()];
        let mut remaining = available_size;
        let mut grow_indices = Vec::new();

        // 1. Allocate Fixed and Percentage
        for (i, &constraint) in self.constraints.iter().enumerate() {
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

        // 2. Distribute remaining space to flexible constraints (Min, Max, Ratio)
        if remaining > 0 && !grow_indices.is_empty() {
            // Simple distribution: equal share for non-ratio, proportional for ratio
            // If we have both, we need a policy.
            // Let's assume Min/Max get equal share of remainder, Ratio gets ratio.
            // Actually, "Min" means "At least X, then grow".
            
            // Re-evaluating distribution strategy:
            // Calculate total weight. 
            // Min/Max act as weight=1 (equal share) unless Ratio is present?
            // Usually Ratio is explicit. Min/Max are "flex".
            
            // Let's assign weight 1 to Min/Max, and derived weight to Ratio.
            // But Ratio(1, 2) means 50% of *available* (which is remaining?).
            
            // Simplified approach: 
            // - Sum ratios.
            // - Distribute remaining proportionally.
            
            let mut total_weight = 0u64;
            for &i in &grow_indices {
                match self.constraints[i] {
                    Constraint::Ratio(n, d) => total_weight += n as u64 * 100 / d.max(1) as u64,
                    _ => total_weight += 100, // Treat others as Ratio(1, 1) effectively? No.
                }
            }
            
            // If total_weight is 0 (shouldn't happen if indices not empty), fix.
            if total_weight == 0 { total_weight = 1; }

            // Distribute
            let space_to_distribute = remaining;
            let mut allocated = 0;
            
            for (idx, &i) in grow_indices.iter().enumerate() {
                let weight = match self.constraints[i] {
                    Constraint::Ratio(n, d) => n as u64 * 100 / d.max(1) as u64,
                    _ => 100,
                };
                
                // Last item gets the rest to ensure exact sum
                let size = if idx == grow_indices.len() - 1 {
                    space_to_distribute - allocated
                } else {
                    let s = (space_to_distribute as u64 * weight / total_weight) as u16;
                    min(s, space_to_distribute - allocated)
                };
                
                sizes[i] += size;
                allocated += size;
            }
        }

        // 3. Clamp Max constraints
        for (i, &constraint) in self.constraints.iter().enumerate() {
            if let Constraint::Max(max_size) = constraint {
                sizes[i] = sizes[i].min(max_size);
            }
        }

        sizes
    }

    fn sizes_to_rects(&self, area: Rect, sizes: &[u16]) -> Vec<Rect> {
        let mut rects = Vec::with_capacity(sizes.len());
        let mut current_pos = match self.direction {
            Direction::Horizontal => area.x,
            Direction::Vertical => area.y,
        };

        for &size in sizes {
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
            current_pos = current_pos.saturating_add(size).saturating_add(self.gap);
        }

        // Apply alignment if there is leftover space (because of Max clamping or similar)
        // Note: The current implementation just places them sequentially. 
        // Real alignment would shift `current_pos` start or spacing.
        // For v1, Start alignment is implicit.
        
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_split() {
        let flex = Flex::horizontal()
            .constraints([Constraint::Fixed(10), Constraint::Fixed(20)]);
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
}
