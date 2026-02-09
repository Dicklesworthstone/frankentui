//! Parsed Doom map with resolved cross-references.
//!
//! Converts raw WAD data into a usable map structure with f32 coordinates.

use super::wad::WadFile;
use super::wad_types::*;

/// A fully parsed Doom map ready for rendering and gameplay.
#[derive(Debug, Clone)]
pub struct DoomMap {
    pub name: String,
    pub vertices: Vec<Vertex>,
    pub linedefs: Vec<LineDef>,
    pub sidedefs: Vec<SideDef>,
    pub sectors: Vec<Sector>,
    pub segs: Vec<Seg>,
    pub subsectors: Vec<SubSector>,
    pub nodes: Vec<Node>,
    pub things: Vec<Thing>,
}

/// Map vertex in f32 coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
}

/// Resolved linedef with vertex positions and sidedef references.
#[derive(Debug, Clone)]
pub struct LineDef {
    pub v1: usize,
    pub v2: usize,
    pub flags: u16,
    pub special: u16,
    pub tag: u16,
    pub front_sidedef: Option<usize>,
    pub back_sidedef: Option<usize>,
}

impl LineDef {
    #[inline]
    pub fn is_two_sided(&self) -> bool {
        self.flags & ML_TWOSIDED != 0
    }

    #[inline]
    pub fn is_blocking(&self) -> bool {
        self.flags & ML_BLOCKING != 0
    }

    /// Get the front sector index (via front sidedef).
    pub fn front_sector(&self, sidedefs: &[SideDef]) -> Option<usize> {
        self.front_sidedef.map(|s| sidedefs[s].sector)
    }

    /// Get the back sector index (via back sidedef).
    pub fn back_sector(&self, sidedefs: &[SideDef]) -> Option<usize> {
        self.back_sidedef.map(|s| sidedefs[s].sector)
    }
}

/// Resolved sidedef.
#[derive(Debug, Clone)]
pub struct SideDef {
    pub x_offset: f32,
    pub y_offset: f32,
    pub upper_texture: String,
    pub lower_texture: String,
    pub middle_texture: String,
    pub sector: usize,
}

/// Resolved sector.
#[derive(Debug, Clone)]
pub struct Sector {
    pub floor_height: f32,
    pub ceiling_height: f32,
    pub floor_texture: String,
    pub ceiling_texture: String,
    pub light_level: u16,
    pub special: u16,
    pub tag: u16,
}

impl Sector {
    /// Check if this sector has a sky ceiling.
    pub fn is_sky_ceiling(&self) -> bool {
        self.ceiling_texture == "F_SKY1"
    }
}

/// Resolved seg (wall segment in BSP).
#[derive(Debug, Clone, Copy)]
pub struct Seg {
    pub v1: usize,
    pub v2: usize,
    pub angle: f32,
    pub linedef: usize,
    /// 0 = same direction as linedef, 1 = opposite.
    pub direction: u16,
    pub offset: f32,
}

/// Resolved subsector.
#[derive(Debug, Clone, Copy)]
pub struct SubSector {
    pub num_segs: u16,
    pub first_seg: usize,
}

/// Resolved BSP node.
#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Partition line start.
    pub x: f32,
    pub y: f32,
    /// Partition line direction.
    pub dx: f32,
    pub dy: f32,
    /// Bounding boxes [top, bottom, left, right].
    pub bbox_right: [f32; 4],
    pub bbox_left: [f32; 4],
    /// Right child: Some(node_index) or None for subsector leaf.
    pub right_child: NodeChild,
    /// Left child.
    pub left_child: NodeChild,
}

/// A BSP node child: either another node or a subsector leaf.
#[derive(Debug, Clone, Copy)]
pub enum NodeChild {
    Node(usize),
    SubSector(usize),
}

/// A thing (object/entity) placement.
#[derive(Debug, Clone, Copy)]
pub struct Thing {
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    pub thing_type: u16,
    pub flags: u16,
}

impl DoomMap {
    /// Load a map from a WAD file by name (e.g., "E1M1" or "MAP01").
    pub fn load(wad: &WadFile, map_name: &str) -> Result<Self, super::wad::WadError> {
        let map_idx = wad
            .find_lump(map_name)
            .ok_or_else(|| super::wad::WadError::LumpNotFound(map_name.to_string()))?;

        // Map lumps follow the marker in a specific order
        let things_idx = wad
            .find_lump_after("THINGS", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("THINGS".into()))?;
        let linedefs_idx = wad
            .find_lump_after("LINEDEFS", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("LINEDEFS".into()))?;
        let sidedefs_idx = wad
            .find_lump_after("SIDEDEFS", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("SIDEDEFS".into()))?;
        let vertexes_idx = wad
            .find_lump_after("VERTEXES", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("VERTEXES".into()))?;
        let segs_idx = wad
            .find_lump_after("SEGS", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("SEGS".into()))?;
        let ssectors_idx = wad
            .find_lump_after("SSECTORS", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("SSECTORS".into()))?;
        let nodes_idx = wad
            .find_lump_after("NODES", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("NODES".into()))?;
        let sectors_idx = wad
            .find_lump_after("SECTORS", map_idx)
            .ok_or_else(|| super::wad::WadError::LumpNotFound("SECTORS".into()))?;

        // Parse raw data
        let raw_verts = wad.parse_vertices(vertexes_idx);
        let raw_linedefs = wad.parse_linedefs(linedefs_idx);
        let raw_sidedefs = wad.parse_sidedefs(sidedefs_idx);
        let raw_sectors = wad.parse_sectors(sectors_idx);
        let raw_segs = wad.parse_segs(segs_idx);
        let raw_ssectors = wad.parse_subsectors(ssectors_idx);
        let raw_nodes = wad.parse_nodes(nodes_idx);
        let raw_things = wad.parse_things(things_idx);

        // Convert to resolved types
        let vertices: Vec<Vertex> = raw_verts
            .iter()
            .map(|v| Vertex {
                x: v.x as f32,
                y: v.y as f32,
            })
            .collect();

        let sidedefs: Vec<SideDef> = raw_sidedefs
            .iter()
            .map(|s| SideDef {
                x_offset: s.x_offset as f32,
                y_offset: s.y_offset as f32,
                upper_texture: s.upper_name(),
                lower_texture: s.lower_name(),
                middle_texture: s.middle_name(),
                sector: s.sector as usize,
            })
            .collect();

        let linedefs: Vec<LineDef> = raw_linedefs
            .iter()
            .map(|l| LineDef {
                v1: l.v1 as usize,
                v2: l.v2 as usize,
                flags: l.flags,
                special: l.special,
                tag: l.tag,
                front_sidedef: if l.right_sidedef == 0xFFFF {
                    None
                } else {
                    Some(l.right_sidedef as usize)
                },
                back_sidedef: if l.left_sidedef == 0xFFFF {
                    None
                } else {
                    Some(l.left_sidedef as usize)
                },
            })
            .collect();

        let sectors: Vec<Sector> = raw_sectors
            .iter()
            .map(|s| Sector {
                floor_height: s.floor_height as f32,
                ceiling_height: s.ceiling_height as f32,
                floor_texture: s.floor_name(),
                ceiling_texture: s.ceiling_name(),
                light_level: s.light_level,
                special: s.special,
                tag: s.tag,
            })
            .collect();

        let segs: Vec<Seg> = raw_segs
            .iter()
            .map(|s| Seg {
                v1: s.v1 as usize,
                v2: s.v2 as usize,
                angle: (s.angle as f32) * std::f32::consts::PI / 32768.0,
                linedef: s.linedef as usize,
                direction: s.direction,
                offset: s.offset as f32,
            })
            .collect();

        let subsectors: Vec<SubSector> = raw_ssectors
            .iter()
            .map(|s| SubSector {
                num_segs: s.num_segs,
                first_seg: s.first_seg as usize,
            })
            .collect();

        let nodes: Vec<Node> = raw_nodes
            .iter()
            .map(|n| Node {
                x: n.x as f32,
                y: n.y as f32,
                dx: n.dx as f32,
                dy: n.dy as f32,
                bbox_right: [
                    n.bbox_right[0] as f32,
                    n.bbox_right[1] as f32,
                    n.bbox_right[2] as f32,
                    n.bbox_right[3] as f32,
                ],
                bbox_left: [
                    n.bbox_left[0] as f32,
                    n.bbox_left[1] as f32,
                    n.bbox_left[2] as f32,
                    n.bbox_left[3] as f32,
                ],
                right_child: parse_child(n.right_child),
                left_child: parse_child(n.left_child),
            })
            .collect();

        let things: Vec<Thing> = raw_things
            .iter()
            .map(|t| Thing {
                x: t.x as f32,
                y: t.y as f32,
                angle: (t.angle as f32) * std::f32::consts::PI / 180.0,
                thing_type: t.thing_type,
                flags: t.flags,
            })
            .collect();

        Ok(DoomMap {
            name: map_name.to_uppercase(),
            vertices,
            linedefs,
            sidedefs,
            sectors,
            segs,
            subsectors,
            nodes,
            things,
        })
    }

    /// Find the player 1 start position.
    pub fn player_start(&self) -> Option<(f32, f32, f32)> {
        self.things
            .iter()
            .find(|t| t.thing_type == THING_PLAYER1)
            .map(|t| (t.x, t.y, t.angle))
    }

    /// Find which subsector contains a point using BSP traversal.
    pub fn point_in_subsector(&self, x: f32, y: f32) -> usize {
        if self.nodes.is_empty() {
            return 0;
        }
        let mut node_idx = self.nodes.len() - 1; // Start at root
        loop {
            let node = &self.nodes[node_idx];
            let side = super::geometry::point_on_side(x, y, node.x, node.y, node.dx, node.dy);
            let child = if side {
                node.right_child
            } else {
                node.left_child
            };
            match child {
                NodeChild::SubSector(ss) => return ss,
                NodeChild::Node(n) => node_idx = n,
            }
        }
    }

    /// Get the sector that contains a point.
    pub fn point_sector(&self, x: f32, y: f32) -> Option<&Sector> {
        let ss_idx = self.point_in_subsector(x, y);
        let ss = &self.subsectors[ss_idx];
        if ss.num_segs == 0 {
            return None;
        }
        let seg = &self.segs[ss.first_seg];
        let linedef = &self.linedefs[seg.linedef];
        let sidedef_idx = if seg.direction == 0 {
            linedef.front_sidedef?
        } else {
            linedef.back_sidedef?
        };
        Some(&self.sectors[self.sidedefs[sidedef_idx].sector])
    }
}

fn parse_child(value: u16) -> NodeChild {
    if value & NF_SUBSECTOR != 0 {
        NodeChild::SubSector((value & !NF_SUBSECTOR) as usize)
    } else {
        NodeChild::Node(value as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_child_node() {
        let NodeChild::Node(n) = parse_child(42) else {
            unreachable!("Expected Node variant");
        };
        assert_eq!(n, 42);
    }

    #[test]
    fn parse_child_subsector() {
        let NodeChild::SubSector(s) = parse_child(0x8005) else {
            unreachable!("Expected SubSector variant");
        };
        assert_eq!(s, 5);
    }

    #[test]
    fn parse_child_node_zero() {
        let NodeChild::Node(n) = parse_child(0) else {
            unreachable!("Expected Node variant");
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn parse_child_subsector_zero() {
        let NodeChild::SubSector(s) = parse_child(NF_SUBSECTOR) else {
            unreachable!("Expected SubSector variant");
        };
        assert_eq!(s, 0);
    }

    #[test]
    fn linedef_is_two_sided() {
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: ML_TWOSIDED,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        assert!(ld.is_two_sided());
    }

    #[test]
    fn linedef_not_two_sided() {
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        assert!(!ld.is_two_sided());
    }

    #[test]
    fn linedef_is_blocking() {
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: ML_BLOCKING,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        assert!(ld.is_blocking());
    }

    #[test]
    fn linedef_not_blocking() {
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: ML_TWOSIDED,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        assert!(!ld.is_blocking());
    }

    #[test]
    fn linedef_front_sector_some() {
        let sidedefs = vec![SideDef {
            x_offset: 0.0,
            y_offset: 0.0,
            upper_texture: String::new(),
            lower_texture: String::new(),
            middle_texture: String::new(),
            sector: 3,
        }];
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: Some(0),
            back_sidedef: None,
        };
        assert_eq!(ld.front_sector(&sidedefs), Some(3));
    }

    #[test]
    fn linedef_front_sector_none() {
        let sidedefs: Vec<SideDef> = vec![];
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        assert_eq!(ld.front_sector(&sidedefs), None);
    }

    #[test]
    fn linedef_back_sector_some() {
        let sidedefs = vec![SideDef {
            x_offset: 0.0,
            y_offset: 0.0,
            upper_texture: String::new(),
            lower_texture: String::new(),
            middle_texture: String::new(),
            sector: 7,
        }];
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: 0,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: Some(0),
        };
        assert_eq!(ld.back_sector(&sidedefs), Some(7));
    }

    #[test]
    fn linedef_combined_flags() {
        let ld = LineDef {
            v1: 0,
            v2: 1,
            flags: ML_BLOCKING | ML_TWOSIDED,
            special: 0,
            tag: 0,
            front_sidedef: None,
            back_sidedef: None,
        };
        assert!(ld.is_blocking());
        assert!(ld.is_two_sided());
    }

    #[test]
    fn sector_is_sky_ceiling() {
        let s = Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: String::new(),
            ceiling_texture: "F_SKY1".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        };
        assert!(s.is_sky_ceiling());
    }

    #[test]
    fn sector_not_sky_ceiling() {
        let s = Sector {
            floor_height: 0.0,
            ceiling_height: 128.0,
            floor_texture: String::new(),
            ceiling_texture: "FLAT1".into(),
            light_level: 200,
            special: 0,
            tag: 0,
        };
        assert!(!s.is_sky_ceiling());
    }

    #[test]
    fn point_in_subsector_empty_nodes() {
        let map = DoomMap {
            name: "TEST".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![],
            nodes: vec![],
            things: vec![],
        };
        assert_eq!(map.point_in_subsector(0.0, 0.0), 0);
    }

    #[test]
    fn vertex_coords() {
        let v = Vertex { x: 10.5, y: -20.3 };
        assert!((v.x - 10.5).abs() < f32::EPSILON);
        assert!((v.y + 20.3).abs() < f32::EPSILON);
    }

    #[test]
    fn thing_fields() {
        let t = Thing {
            x: 100.0,
            y: 200.0,
            angle: 1.5,
            thing_type: THING_PLAYER1,
            flags: 0,
        };
        assert_eq!(t.thing_type, THING_PLAYER1);
        assert!((t.angle - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn seg_fields() {
        let s = Seg {
            v1: 0,
            v2: 1,
            angle: std::f32::consts::PI,
            linedef: 5,
            direction: 0,
            offset: 10.0,
        };
        assert_eq!(s.linedef, 5);
        assert_eq!(s.direction, 0);
    }

    #[test]
    fn subsector_fields() {
        let ss = SubSector {
            num_segs: 4,
            first_seg: 10,
        };
        assert_eq!(ss.num_segs, 4);
        assert_eq!(ss.first_seg, 10);
    }

    #[test]
    fn player_start_found() {
        let map = DoomMap {
            name: "TEST".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![],
            nodes: vec![],
            things: vec![
                Thing {
                    x: 50.0,
                    y: 60.0,
                    angle: 0.0,
                    thing_type: 0,
                    flags: 0,
                },
                Thing {
                    x: 100.0,
                    y: 200.0,
                    angle: 1.5,
                    thing_type: THING_PLAYER1,
                    flags: 0x07,
                },
            ],
        };
        let start = map.player_start();
        assert!(start.is_some());
        let (x, y, a) = start.unwrap();
        assert!((x - 100.0).abs() < f32::EPSILON);
        assert!((y - 200.0).abs() < f32::EPSILON);
        assert!((a - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn player_start_not_found() {
        let map = DoomMap {
            name: "TEST".into(),
            vertices: vec![],
            linedefs: vec![],
            sidedefs: vec![],
            sectors: vec![],
            segs: vec![],
            subsectors: vec![],
            nodes: vec![],
            things: vec![Thing {
                x: 50.0,
                y: 60.0,
                angle: 0.0,
                thing_type: 0,
                flags: 0,
            }],
        };
        assert!(map.player_start().is_none());
    }
}
