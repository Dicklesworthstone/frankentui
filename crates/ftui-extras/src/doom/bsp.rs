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
