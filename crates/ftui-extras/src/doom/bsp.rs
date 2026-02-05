//! BSP tree traversal for Doom's front-to-back rendering.
//!
//! Traverses the BSP tree visiting subsectors in front-to-back order
//! relative to the viewer's position.

use super::geometry::point_on_side;
use super::map::{DoomMap, NodeChild};

/// Callback for processing a subsector during BSP traversal.
/// Return `true` to continue, `false` to early-exit.
pub type SubSectorVisitor<'a> = &'a mut dyn FnMut(usize) -> bool;

/// Traverse the BSP tree front-to-back from the given viewpoint.
/// Calls `visitor` for each subsector in front-to-back order.
pub fn bsp_traverse(map: &DoomMap, view_x: f32, view_y: f32, visitor: SubSectorVisitor<'_>) {
    if map.nodes.is_empty() {
        // Degenerate: single subsector map
        if !map.subsectors.is_empty() {
            visitor(0);
        }
        return;
    }
    traverse_node(map, map.nodes.len() - 1, view_x, view_y, visitor);
}

/// Recursively traverse a BSP node.
/// Returns false to signal early termination.
fn traverse_node(
    map: &DoomMap,
    node_idx: usize,
    view_x: f32,
    view_y: f32,
    visitor: SubSectorVisitor<'_>,
) -> bool {
    let node = &map.nodes[node_idx];

    // Determine which side of the partition line the viewer is on
    let on_front = point_on_side(view_x, view_y, node.x, node.y, node.dx, node.dy);

    // Visit the near side first (front-to-back)
    let (near, far) = if on_front {
        (node.right_child, node.left_child)
    } else {
        (node.left_child, node.right_child)
    };

    // Process near child
    if !visit_child(map, near, view_x, view_y, visitor) {
        return false;
    }

    // Process far child
    visit_child(map, far, view_x, view_y, visitor)
}

/// Visit a single BSP child node (either node or subsector).
fn visit_child(
    map: &DoomMap,
    child: NodeChild,
    view_x: f32,
    view_y: f32,
    visitor: SubSectorVisitor<'_>,
) -> bool {
    match child {
        NodeChild::SubSector(ss_idx) => visitor(ss_idx),
        NodeChild::Node(node_idx) => traverse_node(map, node_idx, view_x, view_y, visitor),
    }
}

/// Check if a bounding box is potentially visible from the viewpoint.
/// The bbox is [top, bottom, left, right] in map coordinates.
#[allow(dead_code)]
pub fn bbox_visible(view_x: f32, view_y: f32, view_angle: f32, fov: f32, bbox: &[f32; 4]) -> bool {
    let top = bbox[0];
    let bottom = bbox[1];
    let left = bbox[2];
    let right = bbox[3];

    // Quick reject: is the viewer inside the bbox?
    if view_x >= left && view_x <= right && view_y >= bottom && view_y <= top {
        return true;
    }

    // Check if any corner of the bbox is within the FOV
    let half_fov = fov / 2.0;
    let corners = [(left, top), (right, top), (right, bottom), (left, bottom)];

    for &(cx, cy) in &corners {
        let angle = (cy - view_y).atan2(cx - view_x);
        let mut diff = angle - view_angle;
        // Normalize to [-PI, PI]
        while diff > std::f32::consts::PI {
            diff -= std::f32::consts::TAU;
        }
        while diff < -std::f32::consts::PI {
            diff += std::f32::consts::TAU;
        }
        if diff.abs() <= half_fov {
            return true;
        }
    }

    // Also check if the bbox spans across the view direction
    // (viewer looking through the box edge-on)
    true // Conservative: always render if corners aren't clearly excluded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_visible_inside() {
        assert!(bbox_visible(
            5.0,
            5.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
            &[10.0, 0.0, 0.0, 10.0]
        ));
    }
}
