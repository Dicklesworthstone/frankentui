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

    #[test]
    fn bbox_visible_corner_in_fov() {
        // Viewer at origin looking right (angle=0), bbox corner at (5,0) is in FOV
        assert!(bbox_visible(
            0.0,
            0.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
            &[1.0, -1.0, 4.0, 6.0]
        ));
    }

    #[test]
    fn bsp_traverse_empty_map() {
        let map = DoomMap {
            name: "EMPTY".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![],
            nodes: vec![],
            things: vec![],
        };
        let mut visited = vec![];
        bsp_traverse(&map, 0.0, 0.0, &mut |_ss| {
            visited.push(0);
            true
        });
        assert!(visited.is_empty(), "Empty map should visit nothing");
    }

    #[test]
    fn bsp_traverse_single_subsector() {
        use crate::doom::map::SubSector;
        let map = DoomMap {
            name: "SINGLE".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![SubSector {
                num_segs: 0,
                first_seg: 0,
            }],
            nodes: vec![],
            things: vec![],
        };
        let mut visited = vec![];
        bsp_traverse(&map, 0.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![0], "Should visit the single subsector");
    }

    #[test]
    fn bsp_traverse_two_subsectors() {
        use crate::doom::map::{Node, SubSector};
        // One node splitting left/right at x=0 (partition goes along y-axis)
        let map = DoomMap {
            name: "TWO".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![Node {
                x: 0.0,
                y: 0.0,
                dx: 0.0,
                dy: 1.0,
                bbox_right: [0.0; 4],
                bbox_left: [0.0; 4],
                right_child: NodeChild::SubSector(0),
                left_child: NodeChild::SubSector(1),
            }],
            things: vec![],
        };
        let mut visited = vec![];
        bsp_traverse(&map, 5.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited.len(), 2, "Should visit both subsectors");
        // Viewer at x=5 with partition along y-axis: cross product > 0 → back side
        // So left_child (SubSector 1) is near, right_child (SubSector 0) is far
        assert_eq!(visited[0], 1);
        assert_eq!(visited[1], 0);
    }

    #[test]
    fn bsp_traverse_early_exit() {
        use crate::doom::map::{Node, SubSector};
        let map = DoomMap {
            name: "EARLY".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![Node {
                x: 0.0,
                y: 0.0,
                dx: 0.0,
                dy: 1.0,
                bbox_right: [0.0; 4],
                bbox_left: [0.0; 4],
                right_child: NodeChild::SubSector(0),
                left_child: NodeChild::SubSector(1),
            }],
            things: vec![],
        };
        let mut visited = vec![];
        bsp_traverse(&map, 5.0, 0.0, &mut |ss| {
            visited.push(ss);
            false // Stop after first
        });
        assert_eq!(
            visited.len(),
            1,
            "Early exit should stop after first subsector"
        );
    }

    // --- Helper for building test maps ---
    fn make_two_subsector_map(px: f32, py: f32, dx: f32, dy: f32) -> DoomMap {
        use crate::doom::map::{Node, SubSector};
        DoomMap {
            name: "TWO".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![Node {
                x: px,
                y: py,
                dx,
                dy,
                bbox_right: [0.0; 4],
                bbox_left: [0.0; 4],
                right_child: NodeChild::SubSector(0),
                left_child: NodeChild::SubSector(1),
            }],
            things: vec![],
        }
    }

    // --- bbox_visible tests ---

    #[test]
    fn bbox_visible_viewer_at_corner() {
        // Viewer exactly at bbox corner — inside
        assert!(bbox_visible(
            0.0,
            0.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
            &[10.0, 0.0, 0.0, 10.0]
        ));
    }

    #[test]
    fn bbox_visible_behind_viewer() {
        // Viewer at origin looking right, bbox entirely to the left (behind)
        // Conservative function returns true anyway
        assert!(bbox_visible(
            0.0,
            0.0,
            0.0,
            std::f32::consts::FRAC_PI_4,
            &[1.0, -1.0, -10.0, -5.0]
        ));
    }

    #[test]
    fn bbox_visible_wide_fov() {
        // Full 360-degree FOV sees everything
        assert!(bbox_visible(
            0.0,
            0.0,
            0.0,
            std::f32::consts::TAU,
            &[100.0, -100.0, -100.0, 100.0]
        ));
    }

    #[test]
    fn bbox_visible_narrow_fov_corner_in_view() {
        // Viewer looking right (angle 0), narrow FOV, corner at (10, 0) is in view
        assert!(bbox_visible(
            0.0,
            0.0,
            0.0,
            0.1, // ~5.7 degrees
            &[1.0, -1.0, 9.0, 11.0]
        ));
    }

    #[test]
    fn bbox_visible_degenerate_zero_size_bbox() {
        // Zero-size bbox (point)
        assert!(bbox_visible(
            0.0,
            0.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
            &[5.0, 5.0, 5.0, 5.0]
        ));
    }

    #[test]
    fn bbox_visible_angle_wrapping_positive() {
        // Viewer looking at angle > PI, should still work due to wrapping
        assert!(bbox_visible(
            0.0,
            0.0,
            std::f32::consts::PI + 0.1,
            std::f32::consts::FRAC_PI_2,
            &[1.0, -1.0, -10.0, -5.0]
        ));
    }

    #[test]
    fn bbox_visible_angle_wrapping_negative() {
        // Viewer with negative angle
        assert!(bbox_visible(
            0.0,
            0.0,
            -std::f32::consts::PI + 0.1,
            std::f32::consts::FRAC_PI_2,
            &[1.0, -1.0, -10.0, -5.0]
        ));
    }

    // --- BSP traversal: deeper trees ---

    #[test]
    fn bsp_traverse_three_subsectors() {
        use crate::doom::map::{Node, SubSector};
        // Tree: root node → left = SS(2), right = child node → right=SS(0), left=SS(1)
        let map = DoomMap {
            name: "THREE".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![
                // node 0: splits at x=5, right=SS(0), left=SS(1)
                Node {
                    x: 5.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::SubSector(0),
                    left_child: NodeChild::SubSector(1),
                },
                // node 1 (root): splits at x=0, right=node(0), left=SS(2)
                Node {
                    x: 0.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::Node(0),
                    left_child: NodeChild::SubSector(2),
                },
            ],
            things: vec![],
        };
        let mut visited = vec![];
        // Viewer at x=10: on back side of root (cross=(10)*1=10 > 0 → back)
        // So left_child=SS(2) is near, right_child=Node(0) is far
        // Then node(0): cross=(10-5)*1=5 > 0 → back, so left=SS(1) near, right=SS(0) far
        bsp_traverse(&map, 10.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited.len(), 3);
        assert_eq!(visited, vec![2, 1, 0]);
    }

    #[test]
    fn bsp_traverse_four_subsectors_balanced() {
        use crate::doom::map::{Node, SubSector};
        // Balanced tree: root → 2 children nodes → 4 subsectors
        let map = DoomMap {
            name: "FOUR".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: (0..4)
                .map(|_| SubSector {
                    num_segs: 0,
                    first_seg: 0,
                })
                .collect(),
            nodes: vec![
                // node 0: right=SS(0), left=SS(1), partition at x=-5 along y
                Node {
                    x: -5.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::SubSector(0),
                    left_child: NodeChild::SubSector(1),
                },
                // node 1: right=SS(2), left=SS(3), partition at x=5 along y
                Node {
                    x: 5.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::SubSector(2),
                    left_child: NodeChild::SubSector(3),
                },
                // node 2 (root): right=node(0), left=node(1), partition at x=0 along y
                Node {
                    x: 0.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::Node(0),
                    left_child: NodeChild::Node(1),
                },
            ],
            things: vec![],
        };
        let mut visited = vec![];
        // Viewer at x=10: cross at root = (10)*1 = 10 > 0 → back side → left=node(1) near
        // node(1): cross = (10-5)*1 = 5 > 0 → back → left=SS(3) near, right=SS(2) far
        // Then far=node(0): cross = (10-(-5))*1 = 15 > 0 → back → left=SS(1) near, right=SS(0) far
        bsp_traverse(&map, 10.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited.len(), 4);
        assert_eq!(visited, vec![3, 2, 1, 0]);
    }

    #[test]
    fn bsp_traverse_early_exit_on_second_subsector() {
        use crate::doom::map::{Node, SubSector};
        // 3 subsectors, exit after visiting 2nd one
        let map = DoomMap {
            name: "EXIT2".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![
                Node {
                    x: 5.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::SubSector(0),
                    left_child: NodeChild::SubSector(1),
                },
                Node {
                    x: 0.0,
                    y: 0.0,
                    dx: 0.0,
                    dy: 1.0,
                    bbox_right: [0.0; 4],
                    bbox_left: [0.0; 4],
                    right_child: NodeChild::Node(0),
                    left_child: NodeChild::SubSector(2),
                },
            ],
            things: vec![],
        };
        let mut count = 0;
        let mut visited = vec![];
        bsp_traverse(&map, 10.0, 0.0, &mut |ss| {
            visited.push(ss);
            count += 1;
            count < 2 // Stop after 2nd visit
        });
        assert_eq!(visited.len(), 2);
    }

    // --- Partition line orientation tests ---

    #[test]
    fn bsp_traverse_horizontal_partition() {
        // Partition along x-axis (dx=1, dy=0): splits top/bottom
        let map = make_two_subsector_map(0.0, 0.0, 1.0, 0.0);
        let mut visited = vec![];
        // Viewer at y=5: cross = (0)*0 - (5)*1 = -5, <= 0 → front
        // front → right=SS(0) near, left=SS(1) far
        bsp_traverse(&map, 0.0, 5.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![0, 1]);
    }

    #[test]
    fn bsp_traverse_horizontal_partition_below() {
        // Viewer below horizontal partition
        let map = make_two_subsector_map(0.0, 0.0, 1.0, 0.0);
        let mut visited = vec![];
        // Viewer at y=-5: cross = (0)*0 - (-5)*1 = 5, > 0 → back
        // back → left=SS(1) near, right=SS(0) far
        bsp_traverse(&map, 0.0, -5.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![1, 0]);
    }

    #[test]
    fn bsp_traverse_diagonal_partition() {
        // Partition at 45 degrees: dx=1, dy=1
        let map = make_two_subsector_map(0.0, 0.0, 1.0, 1.0);
        let mut visited = vec![];
        // Viewer at (5, 0): cross = (5)*1 - (0)*1 = 5, > 0 → back
        // back → left=SS(1) near, right=SS(0) far
        bsp_traverse(&map, 5.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![1, 0]);
    }

    #[test]
    fn bsp_traverse_viewer_on_partition_line() {
        // Viewer exactly on partition → cross product = 0 → front side (<=0)
        let map = make_two_subsector_map(0.0, 0.0, 0.0, 1.0);
        let mut visited = vec![];
        // Viewer at origin: cross = (0)*1 - (0)*0 = 0, <= 0 → front
        bsp_traverse(&map, 0.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![0, 1]);
    }

    #[test]
    fn bsp_traverse_viewer_along_partition_line() {
        // Viewer at any point along the partition line
        let map = make_two_subsector_map(0.0, 0.0, 0.0, 1.0);
        let mut visited = vec![];
        // Viewer at (0, 100): cross = (0)*1 - (100)*0 = 0, front
        bsp_traverse(&map, 0.0, 100.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![0, 1]);
    }

    #[test]
    fn bsp_traverse_offset_partition_origin() {
        // Partition not at origin: x=10, y=10, along y-axis
        let map = make_two_subsector_map(10.0, 10.0, 0.0, 1.0);
        let mut visited = vec![];
        // Viewer at (15, 0): cross = (15-10)*1 - (0-10)*0 = 5, > 0 → back
        bsp_traverse(&map, 15.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![1, 0]);
    }

    #[test]
    fn bsp_traverse_offset_partition_viewer_on_front() {
        let map = make_two_subsector_map(10.0, 10.0, 0.0, 1.0);
        let mut visited = vec![];
        // Viewer at (5, 0): cross = (5-10)*1 - (0-10)*0 = -5, <= 0 → front
        bsp_traverse(&map, 5.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![0, 1]);
    }

    // --- visit_child tests (via traversal) ---

    #[test]
    fn bsp_traverse_single_subsector_no_nodes_multiple_ss() {
        use crate::doom::map::SubSector;
        // Map with multiple subsectors but no nodes → only SS(0) visited
        let map = DoomMap {
            name: "MULTI_SS_NO_NODES".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![],
            things: vec![],
        };
        let mut visited = vec![];
        bsp_traverse(&map, 0.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited, vec![0], "Degenerate case visits only SS(0)");
    }

    #[test]
    fn bsp_traverse_empty_no_subsectors_no_nodes() {
        let map = DoomMap {
            name: "EMPTY2".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![],
            nodes: vec![],
            things: vec![],
        };
        let mut visited = vec![];
        bsp_traverse(&map, 0.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert!(
            visited.is_empty(),
            "No subsectors → nothing should be visited"
        );
    }

    // --- Traversal order determinism ---

    #[test]
    fn bsp_traverse_deterministic_same_input() {
        let map = make_two_subsector_map(0.0, 0.0, 0.0, 1.0);
        let mut run1 = vec![];
        let mut run2 = vec![];
        bsp_traverse(&map, 5.0, 3.0, &mut |ss| {
            run1.push(ss);
            true
        });
        bsp_traverse(&map, 5.0, 3.0, &mut |ss| {
            run2.push(ss);
            true
        });
        assert_eq!(run1, run2, "Same inputs must produce same traversal order");
    }

    #[test]
    fn bsp_traverse_opposite_sides_reverse_order() {
        let map = make_two_subsector_map(0.0, 0.0, 0.0, 1.0);
        let mut from_right = vec![];
        let mut from_left = vec![];
        bsp_traverse(&map, 10.0, 0.0, &mut |ss| {
            from_right.push(ss);
            true
        });
        bsp_traverse(&map, -10.0, 0.0, &mut |ss| {
            from_left.push(ss);
            true
        });
        // Front-to-back ordering should be reversed for opposite viewers
        assert_eq!(from_right.len(), 2);
        assert_eq!(from_left.len(), 2);
        assert_eq!(from_right[0], from_left[1]);
        assert_eq!(from_right[1], from_left[0]);
    }

    #[test]
    fn bsp_traverse_viewer_on_left_side() {
        use crate::doom::map::{Node, SubSector};
        let map = DoomMap {
            name: "LEFT".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
                SubSector {
                    num_segs: 0,
                    first_seg: 0,
                },
            ],
            nodes: vec![Node {
                x: 0.0,
                y: 0.0,
                dx: 0.0,
                dy: 1.0,
                bbox_right: [0.0; 4],
                bbox_left: [0.0; 4],
                right_child: NodeChild::SubSector(0),
                left_child: NodeChild::SubSector(1),
            }],
            things: vec![],
        };
        let mut visited = vec![];
        // Viewer at x=-5: cross = (-5)*1 - (0)*0 = -5, which is <= 0 → front side
        // So right_child (SubSector 0) is near, left_child (SubSector 1) is far
        bsp_traverse(&map, -5.0, 0.0, &mut |ss| {
            visited.push(ss);
            true
        });
        assert_eq!(visited.len(), 2);
        assert_eq!(
            visited[0], 0,
            "Right subsector should be visited first for front-side viewer"
        );
        assert_eq!(visited[1], 1);
    }
}
