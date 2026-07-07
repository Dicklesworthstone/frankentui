#![forbid(unsafe_code)]

//! Geometric primitives.

/// A 2D size in terminal cells.
///
/// Represents dimensions (width and height) using `u16` for terminal coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Size {
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl Size {
    /// Zero size (0x0).
    pub const ZERO: Self = Self {
        width: 0,
        height: 0,
    };

    /// Maximum size (u16::MAX x u16::MAX).
    ///
    /// Useful as a sentinel for "unbounded" or "as large as needed".
    pub const MAX: Self = Self {
        width: u16::MAX,
        height: u16::MAX,
    };

    /// Create a new size.
    #[inline]
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }

    /// Check if this size has zero area.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Area in cells.
    #[inline]
    pub const fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Clamp width and height to the given maximums.
    #[inline]
    pub const fn clamp_max(&self, max: Size) -> Size {
        Size {
            width: if self.width > max.width {
                max.width
            } else {
                self.width
            },
            height: if self.height > max.height {
                max.height
            } else {
                self.height
            },
        }
    }

    /// Clamp width and height to the given minimums.
    #[inline]
    pub const fn clamp_min(&self, min: Size) -> Size {
        Size {
            width: if self.width < min.width {
                min.width
            } else {
                self.width
            },
            height: if self.height < min.height {
                min.height
            } else {
                self.height
            },
        }
    }
}

impl From<(u16, u16)> for Size {
    fn from((width, height): (u16, u16)) -> Self {
        Self { width, height }
    }
}

impl From<Rect> for Size {
    fn from(rect: Rect) -> Self {
        Self {
            width: rect.width,
            height: rect.height,
        }
    }
}

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

    /// Left edge (inclusive). Alias for `self.x`.
    #[inline]
    pub const fn left(&self) -> u16 {
        self.x
    }

    /// Top edge (inclusive). Alias for `self.y`.
    #[inline]
    pub const fn top(&self) -> u16 {
        self.y
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
    #[inline]
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
    ///
    /// Note: a rectangle's *position* participates even when it is empty —
    /// `Rect::new(100, 100, 0, 0).union(&r)` extends to x/y 100. When
    /// accumulating a union over a collection, seed the fold with the first
    /// element rather than `Rect::default()` (which would pin the result to
    /// the origin).
    #[inline]
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
            Some(Rect::new(
                x,
                y,
                right.saturating_sub(x),
                bottom.saturating_sub(y),
            ))
        } else {
            None
        }
    }
}

/// Sides for padding/margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sides {
    /// Top padding/margin in cells.
    pub top: u16,
    /// Right padding/margin in cells.
    pub right: u16,
    /// Bottom padding/margin in cells.
    pub bottom: u16,
    /// Left padding/margin in cells.
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
    use super::{Rect, Sides, Size};

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

    // --- Rect constructors ---

    #[test]
    fn rect_new_and_default() {
        let r = Rect::new(5, 10, 20, 15);
        assert_eq!(r.x, 5);
        assert_eq!(r.y, 10);
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 15);

        let d = Rect::default();
        assert_eq!(d, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn rect_from_size() {
        let r = Rect::from_size(80, 24);
        assert_eq!(r.x, 0);
        assert_eq!(r.y, 0);
        assert_eq!(r.width, 80);
        assert_eq!(r.height, 24);
    }

    // --- Edge accessors ---

    #[test]
    fn rect_left_top_right_bottom() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!(r.left(), 10);
        assert_eq!(r.top(), 20);
        assert_eq!(r.right(), 40);
        assert_eq!(r.bottom(), 60);
    }

    #[test]
    fn rect_right_bottom_saturating() {
        // Near u16::MAX — should not overflow
        let r = Rect::new(u16::MAX - 5, u16::MAX - 3, 100, 100);
        assert_eq!(r.right(), u16::MAX);
        assert_eq!(r.bottom(), u16::MAX);
    }

    // --- Area and is_empty ---

    #[test]
    fn rect_area() {
        assert_eq!(Rect::new(0, 0, 10, 20).area(), 200);
        assert_eq!(Rect::new(5, 5, 0, 10).area(), 0);
        assert_eq!(Rect::new(0, 0, 1, 1).area(), 1);
    }

    #[test]
    fn rect_is_empty() {
        assert!(Rect::new(0, 0, 0, 0).is_empty());
        assert!(Rect::new(5, 5, 0, 10).is_empty());
        assert!(Rect::new(5, 5, 10, 0).is_empty());
        assert!(!Rect::new(0, 0, 1, 1).is_empty());
    }

    // --- Contains ---

    #[test]
    fn rect_contains_boundary_conditions() {
        let r = Rect::new(0, 0, 5, 5);
        // Top-left corner (inclusive)
        assert!(r.contains(0, 0));
        // Just inside right/bottom edge
        assert!(r.contains(4, 4));
        // Right edge is exclusive
        assert!(!r.contains(5, 0));
        // Bottom edge is exclusive
        assert!(!r.contains(0, 5));
    }

    #[test]
    fn rect_contains_empty_rect() {
        let r = Rect::new(5, 5, 0, 0);
        // Empty rect contains nothing, not even its own origin
        assert!(!r.contains(5, 5));
    }

    // --- Union ---

    #[test]
    fn rect_union_basic() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(3, 3, 5, 5);
        let u = a.union(&b);
        assert_eq!(u, Rect::new(0, 0, 8, 8));
    }

    #[test]
    fn rect_union_disjoint() {
        let a = Rect::new(0, 0, 2, 2);
        let b = Rect::new(10, 10, 3, 3);
        let u = a.union(&b);
        assert_eq!(u, Rect::new(0, 0, 13, 13));
    }

    #[test]
    fn rect_union_contained() {
        let outer = Rect::new(0, 0, 10, 10);
        let inner = Rect::new(2, 2, 3, 3);
        assert_eq!(outer.union(&inner), outer);
        assert_eq!(inner.union(&outer), outer);
    }

    #[test]
    fn rect_union_self() {
        let r = Rect::new(5, 10, 20, 15);
        assert_eq!(r.union(&r), r);
    }

    // --- Intersection ---

    #[test]
    fn rect_intersection_self() {
        let r = Rect::new(5, 5, 10, 10);
        assert_eq!(r.intersection(&r), r);
    }

    #[test]
    fn rect_intersection_contained() {
        let outer = Rect::new(0, 0, 20, 20);
        let inner = Rect::new(5, 5, 5, 5);
        assert_eq!(outer.intersection(&inner), inner);
        assert_eq!(inner.intersection(&outer), inner);
    }

    #[test]
    fn rect_intersection_adjacent_no_overlap() {
        // Rects share an edge but don't overlap (right edge is exclusive)
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(5, 0, 5, 5);
        assert!(a.intersection(&b).is_empty());
    }

    #[test]
    fn rect_intersection_opt_returns_none_for_no_overlap() {
        let a = Rect::new(0, 0, 2, 2);
        let b = Rect::new(5, 5, 2, 2);
        assert_eq!(a.intersection_opt(&b), None);
    }

    #[test]
    fn rect_intersection_opt_returns_some_for_overlap() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(3, 3, 5, 5);
        assert_eq!(a.intersection_opt(&b), Some(Rect::new(3, 3, 2, 2)));
    }

    // --- Inner margin edge cases ---

    #[test]
    fn rect_inner_large_margin_clamps_to_zero() {
        let r = Rect::new(0, 0, 10, 10);
        let inner = r.inner(Sides::all(20));
        // Width/height should clamp to 0 (not underflow)
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    #[test]
    fn rect_inner_zero_margin() {
        let r = Rect::new(5, 10, 20, 30);
        let inner = r.inner(Sides::all(0));
        assert_eq!(inner, r);
    }

    #[test]
    fn rect_inner_asymmetric_margin() {
        let r = Rect::new(0, 0, 20, 20);
        let inner = r.inner(Sides::new(2, 3, 4, 5));
        assert_eq!(inner.x, 5);
        assert_eq!(inner.y, 2);
        assert_eq!(inner.width, 12); // 20 - 5 - 3
        assert_eq!(inner.height, 14); // 20 - 2 - 4
    }

    // --- Sides ---

    #[test]
    fn sides_new_explicit() {
        let s = Sides::new(1, 2, 3, 4);
        assert_eq!(s.top, 1);
        assert_eq!(s.right, 2);
        assert_eq!(s.bottom, 3);
        assert_eq!(s.left, 4);
    }

    #[test]
    fn sides_default_is_zero() {
        let s = Sides::default();
        assert_eq!(s, Sides::new(0, 0, 0, 0));
    }

    #[test]
    fn sides_sums_saturating() {
        let s = Sides::new(u16::MAX, 0, u16::MAX, 0);
        assert_eq!(s.vertical_sum(), u16::MAX);
    }

    // --- Size tests ---

    #[test]
    fn size_new_and_constants() {
        let s = Size::new(80, 24);
        assert_eq!(s.width, 80);
        assert_eq!(s.height, 24);

        assert_eq!(Size::ZERO, Size::new(0, 0));
        assert_eq!(Size::MAX, Size::new(u16::MAX, u16::MAX));
    }

    #[test]
    fn size_default_is_zero() {
        assert_eq!(Size::default(), Size::ZERO);
    }

    #[test]
    fn size_is_empty() {
        assert!(Size::ZERO.is_empty());
        assert!(Size::new(0, 10).is_empty());
        assert!(Size::new(10, 0).is_empty());
        assert!(!Size::new(1, 1).is_empty());
    }

    #[test]
    fn size_area() {
        assert_eq!(Size::new(10, 20).area(), 200);
        assert_eq!(Size::ZERO.area(), 0);
        assert_eq!(Size::new(1, 1).area(), 1);
    }

    #[test]
    fn size_clamp_max() {
        let s = Size::new(100, 50);
        assert_eq!(s.clamp_max(Size::new(80, 40)), Size::new(80, 40));
        assert_eq!(s.clamp_max(Size::new(200, 200)), s);
        assert_eq!(s.clamp_max(Size::new(80, 100)), Size::new(80, 50));
    }

    #[test]
    fn size_clamp_min() {
        let s = Size::new(10, 5);
        assert_eq!(s.clamp_min(Size::new(20, 10)), Size::new(20, 10));
        assert_eq!(s.clamp_min(Size::new(5, 3)), s);
        assert_eq!(s.clamp_min(Size::new(15, 3)), Size::new(15, 5));
    }

    #[test]
    fn size_from_tuple() {
        let s: Size = (80, 24).into();
        assert_eq!(s, Size::new(80, 24));
    }

    #[test]
    fn size_from_rect() {
        let r = Rect::new(5, 10, 80, 24);
        let s: Size = r.into();
        assert_eq!(s, Size::new(80, 24));
    }
}
