//! BSP file format types ported from Quake 1 (id Software GPL, bspfile.h).
//!
//! These match the on-disk format of Quake .bsp files exactly.

/// BSP file header lump descriptor.
#[derive(Debug, Clone, Copy)]
pub struct Lump {
    pub offset: i32,
    pub length: i32,
}

/// BSP file header.
#[derive(Debug, Clone)]
pub struct BspHeader {
    pub version: i32,
    pub lumps: [Lump; super::constants::HEADER_LUMPS],
}

/// On-disk vertex (3 floats).
#[derive(Debug, Clone, Copy)]
pub struct DVertex {
    pub point: [f32; 3],
}

/// On-disk plane.
#[derive(Debug, Clone, Copy)]
pub struct DPlane {
    pub normal: [f32; 3],
    pub dist: f32,
    pub plane_type: i32,
}

/// On-disk BSP node.
#[derive(Debug, Clone, Copy)]
pub struct DNode {
    pub plane_num: i32,
    pub children: [i16; 2], // negative = -(leaf+1)
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub first_face: u16,
    pub num_faces: u16,
}

/// On-disk BSP leaf.
#[derive(Debug, Clone, Copy)]
pub struct DLeaf {
    pub contents: i32,
    pub vis_offset: i32, // -1 = no visibility info
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub first_mark_surface: u16,
    pub num_mark_surfaces: u16,
    pub ambient_level: [u8; 4],
}

/// On-disk face.
#[derive(Debug, Clone, Copy)]
pub struct DFace {
    pub plane_num: i16,
    pub side: i16,
    pub first_edge: i32,
    pub num_edges: i16,
    pub texinfo: i16,
    pub styles: [u8; super::constants::MAXLIGHTMAPS],
    pub light_offset: i32,
}

/// On-disk edge (two vertex indices).
#[derive(Debug, Clone, Copy)]
pub struct DEdge {
    pub v: [u16; 2],
}

/// On-disk clip node (for collision hulls).
#[derive(Debug, Clone, Copy)]
pub struct DClipNode {
    pub plane_num: i32,
    pub children: [i16; 2], // negative = contents
}

/// On-disk texture info.
#[derive(Debug, Clone, Copy)]
pub struct DTexInfo {
    pub vecs: [[f32; 4]; 2], // [s/t][xyz offset]
    pub miptex: i32,
    pub flags: i32,
}

/// On-disk model (world + brush models).
#[derive(Debug, Clone, Copy)]
pub struct DModel {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub origin: [f32; 3],
    pub head_node: [i32; 4], // 4 collision hulls
    pub vis_leafs: i32,
    pub first_face: i32,
    pub num_faces: i32,
}

/// Helper: read i32 little-endian from a byte slice.
#[inline]
pub fn i32_le(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Helper: read u16 little-endian.
#[inline]
pub fn u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Helper: read i16 little-endian.
#[inline]
pub fn i16_le(data: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Helper: read f32 little-endian.
#[inline]
pub fn f32_le(data: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i32_le_roundtrip() {
        let val: i32 = -12345;
        let bytes = val.to_le_bytes();
        assert_eq!(i32_le(&bytes, 0), val);
    }

    #[test]
    fn f32_le_roundtrip() {
        let val: f32 = 3.14159;
        let bytes = val.to_le_bytes();
        assert!((f32_le(&bytes, 0) - val).abs() < 1e-6);
    }
}
