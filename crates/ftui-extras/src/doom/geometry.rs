//! Math utilities for Doom BSP and collision detection.

/// Determine which side of a partition line a point is on.
/// Returns true if the point is on the front (right) side.
#[inline]
pub fn point_on_side(x: f32, y: f32, px: f32, py: f32, dx: f32, dy: f32) -> bool {
    // Cross product: (point - partition_origin) × partition_direction
    // Positive = right side (front), negative = left side (back)
    let cross = (x - px) * dy - (y - py) * dx;
    cross <= 0.0 // Doom convention: <= 0 is front side
}

/// Check if a point is on the front side of a line segment.
#[inline]
pub fn point_on_line_side(x: f32, y: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> bool {
    point_on_side(x, y, x1, y1, x2 - x1, y2 - y1)
}

/// Compute the perpendicular distance from a point to a line segment.
pub fn point_to_segment_dist(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-10 {
        return ((px - x1).powi(2) + (py - y1).powi(2)).sqrt();
    }
    let t = ((px - x1) * dx + (py - y1) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let closest_x = x1 + t * dx;
    let closest_y = y1 + t * dy;
    ((px - closest_x).powi(2) + (py - closest_y).powi(2)).sqrt()
}

/// Line-line intersection. Returns parameter t along the first line if they intersect.
/// First line: (x1,y1) + t*(x2-x1, y2-y1)
/// Second line: (x3,y3) + s*(x4-x3, y4-y3)
#[allow(clippy::too_many_arguments)]
pub fn line_intersection(
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    x4: f32,
    y4: f32,
) -> Option<f32> {
    let dx1 = x2 - x1;
    let dy1 = y2 - y1;
    let dx2 = x4 - x3;
    let dy2 = y4 - y3;

    let denom = dx1 * dy2 - dy1 * dx2;
    if denom.abs() < 1e-10 {
        return None; // Parallel lines
    }

    let t = ((x3 - x1) * dy2 - (y3 - y1) * dx2) / denom;
    let s = ((x3 - x1) * dy1 - (y3 - y1) * dx1) / denom;

    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&s) {
        Some(t)
    } else {
        None
    }
}

/// Compute angle from (x1,y1) to (x2,y2) in radians.
#[inline]
pub fn point_to_angle(x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    (y2 - y1).atan2(x2 - x1)
}

/// Normalize an angle to [0, 2π).
#[inline]
pub fn normalize_angle(angle: f32) -> f32 {
    angle.rem_euclid(std::f32::consts::TAU)
}

/// Compute the shortest angular difference (signed).
#[inline]
pub fn angle_diff(a: f32, b: f32) -> f32 {
    let mut diff = b - a;
    while diff > std::f32::consts::PI {
        diff -= std::f32::consts::TAU;
    }
    while diff < -std::f32::consts::PI {
        diff += std::f32::consts::TAU;
    }
    diff
}

/// Distance between two points.
#[inline]
pub fn dist(x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
}

/// Circle-line segment intersection test.
/// Returns true if a circle at (cx,cy) with radius r intersects the segment.
#[inline]
pub fn circle_intersects_segment(
    cx: f32,
    cy: f32,
    r: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) -> bool {
    point_to_segment_dist(cx, cy, x1, y1, x2, y2) < r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_on_side_basic() {
        // Point (1,1) relative to line from origin going right (dx=1, dy=0)
        // Cross = (1-0)*0 - (1-0)*1 = -1, which is <= 0 → front side (true)
        assert!(point_on_side(1.0, 1.0, 0.0, 0.0, 1.0, 0.0));
        // Point (1,-1): cross = (1-0)*0 - (-1-0)*1 = 1, which is > 0 → back side (false)
        assert!(!point_on_side(1.0, -1.0, 0.0, 0.0, 1.0, 0.0));
    }

    #[test]
    fn line_intersection_basic() {
        // Cross at (0.5, 0.5)
        let t = line_intersection(0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0);
        assert!(t.is_some());
        let t = t.unwrap();
        assert!((t - 0.5).abs() < 1e-5);
    }

    #[test]
    fn parallel_lines_no_intersection() {
        assert!(line_intersection(0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0).is_none());
    }

    #[test]
    fn point_to_angle_right() {
        let a = point_to_angle(0.0, 0.0, 1.0, 0.0);
        assert!(a.abs() < 1e-5);
    }

    #[test]
    fn distance_basic() {
        assert!((dist(0.0, 0.0, 3.0, 4.0) - 5.0).abs() < 1e-5);
    }
}
