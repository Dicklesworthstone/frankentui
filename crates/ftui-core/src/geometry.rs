#![forbid(unsafe_code)]

//! Geometric primitives.

/// A rectangle for scissor regions, layout bounds, and hit testing.
///
/// Uses terminal coordinates (0-indexed, origin at top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge (inclusive).
    pub x: u16,
    /// Top edge (inclusive).
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl Rect {
    /// Create a new rectangle.
    #[inline]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Create a rectangle from origin with given size.
    #[inline]
    pub const fn from_size(width: u16, height: u16) -> Self {
        Self::new(0, 0, width, height)
    }

    /// Right edge (exclusive).
    #[inline]
    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge (exclusive).
    #[inline]
    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Area in cells.
    #[inline]
    pub const fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Check if the rectangle has zero area.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Check if a point is inside the rectangle.
    #[inline]
    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Compute the intersection with another rectangle.
    ///
    /// Returns an empty rectangle if the rectangles don't overlap.
    #[inline]
    pub fn intersection(&self, other: &Rect) -> Rect {
        self.intersection_opt(other).unwrap_or_default()
    }

    /// Create a new rectangle inside the current one with the given margin.
    pub fn inner(&self, margin: Sides) -> Rect {
        let x = self.x.saturating_add(margin.left);
        let y = self.y.saturating_add(margin.top);
        let width = self
            .width
            .saturating_sub(margin.left)
            .saturating_sub(margin.right);
        let height = self
            .height
            .saturating_sub(margin.top)
            .saturating_sub(margin.bottom);

        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// Create a new rectangle that is the union of this rectangle and another.
    ///
    /// The result is the smallest rectangle that contains both.
    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());

        Rect {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }

    /// Compute the intersection with another rectangle, returning `None` if no overlap.
    #[inline]
    pub fn intersection_opt(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        if x < right && y < bottom {
            Some(Rect::new(x, y, right - x, bottom - y))
        } else {
            None
        }
    }
}

/// Sides for padding/margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sides {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Sides {
    /// Create new sides with equal values.
    pub const fn all(val: u16) -> Self {
        Self {
            top: val,
            right: val,
            bottom: val,
            left: val,
        }
    }

    /// Create new sides with horizontal values only.
    pub const fn horizontal(val: u16) -> Self {
        Self {
            top: 0,
            right: val,
            bottom: 0,
            left: val,
        }
    }

    /// Create new sides with vertical values only.
    pub const fn vertical(val: u16) -> Self {
        Self {
            top: val,
            right: 0,
            bottom: val,
            left: 0,
        }
    }

    /// Create new sides with specific values.
    pub const fn new(top: u16, right: u16, bottom: u16, left: u16) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// Sum of left and right.
    #[inline]
    pub const fn horizontal_sum(&self) -> u16 {
        self.left.saturating_add(self.right)
    }

    /// Sum of top and bottom.
    #[inline]
    pub const fn vertical_sum(&self) -> u16 {
        self.top.saturating_add(self.bottom)
    }
}

impl From<u16> for Sides {
    fn from(val: u16) -> Self {
        Self::all(val)
    }
}

impl From<(u16, u16)> for Sides {
    fn from((vertical, horizontal): (u16, u16)) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}

impl From<(u16, u16, u16, u16)> for Sides {
    fn from((top, right, bottom, left): (u16, u16, u16, u16)) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Rect, Sides};

    #[test]
    fn rect_contains_edges() {
        let rect = Rect::new(2, 3, 4, 5);
        assert!(rect.contains(2, 3));
        assert!(rect.contains(5, 7));
        assert!(!rect.contains(6, 3));
        assert!(!rect.contains(2, 8));
    }

    #[test]
    fn rect_intersection_overlaps() {
        let a = Rect::new(0, 0, 4, 4);
        let b = Rect::new(2, 2, 4, 4);
        assert_eq!(a.intersection(&b), Rect::new(2, 2, 2, 2));
    }

    #[test]
    fn rect_intersection_no_overlap_is_empty() {
        let a = Rect::new(0, 0, 2, 2);
        let b = Rect::new(3, 3, 2, 2);
        assert_eq!(a.intersection(&b), Rect::default());
    }

    #[test]
    fn rect_inner_reduces() {
        let rect = Rect::new(0, 0, 10, 10);
        let inner = rect.inner(Sides {
            top: 1,
            right: 2,
            bottom: 3,
            left: 4,
        });
        assert_eq!(inner, Rect::new(4, 1, 4, 6));
    }

    #[test]
    fn sides_constructors_and_conversions() {
        assert_eq!(Sides::all(3), Sides::from(3));
        assert_eq!(
            Sides::horizontal(2),
            Sides {
                top: 0,
                right: 2,
                bottom: 0,
                left: 2,
            }
        );
        assert_eq!(
            Sides::vertical(4),
            Sides {
                top: 4,
                right: 0,
                bottom: 4,
                left: 0,
            }
        );
        assert_eq!(
            Sides::from((1, 2)),
            Sides {
                top: 1,
                right: 2,
                bottom: 1,
                left: 2,
            }
        );
        assert_eq!(
            Sides::from((1, 2, 3, 4)),
            Sides {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4,
            }
        );
    }

    #[test]
    fn sides_sums() {
        let sides = Sides {
            top: 1,
            right: 2,
            bottom: 3,
            left: 4,
        };
        assert_eq!(sides.horizontal_sum(), 6);
        assert_eq!(sides.vertical_sum(), 4);
    }
}
