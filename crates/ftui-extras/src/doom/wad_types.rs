//! Raw binary structures from the WAD file format.
//!
//! All structures are parsed from little-endian binary data matching
//! the original Doom WAD specification.

/// WAD file header (12 bytes).
#[derive(Debug, Clone)]
pub struct WadHeader {
    /// "IWAD" or "PWAD"
    pub identification: [u8; 4],
    /// Number of lumps in the directory.
    pub num_lumps: i32,
    /// Offset to the directory table.
    pub info_table_ofs: i32,
}

/// Directory entry (16 bytes).
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Offset to the lump data in the file.
    pub filepos: i32,
    /// Size of the lump in bytes.
    pub size: i32,
    /// 8-character ASCII name (null-padded).
    pub name: [u8; 8],
}

impl DirEntry {
    /// Get the name as a trimmed uppercase string.
    pub fn name_str(&self) -> String {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(8);
        String::from_utf8_lossy(&self.name[..end]).to_uppercase()
    }
}

/// Vertex (4 bytes): x, y as i16 in map units.
#[derive(Debug, Clone, Copy)]
pub struct RawVertex {
    pub x: i16,
    pub y: i16,
}

/// LineDef (14 bytes): wall segment between two vertices.
#[derive(Debug, Clone, Copy)]
pub struct RawLineDef {
    pub v1: u16,
    pub v2: u16,
    pub flags: u16,
    pub special: u16,
    pub tag: u16,
    pub right_sidedef: u16,
    pub left_sidedef: u16,
}

/// LineDef flags.
pub const ML_BLOCKING: u16 = 1;
pub const ML_TWOSIDED: u16 = 4;
pub const ML_DONTPEGTOP: u16 = 8;
pub const ML_DONTPEGBOTTOM: u16 = 16;
pub const ML_SECRET: u16 = 32;
pub const ML_SOUNDBLOCK: u16 = 64;
pub const ML_DONTDRAW: u16 = 128;
pub const ML_MAPPED: u16 = 256;

/// SideDef (30 bytes): texture info for one side of a linedef.
#[derive(Debug, Clone)]
pub struct RawSideDef {
    pub x_offset: i16,
    pub y_offset: i16,
    pub upper_texture: [u8; 8],
    pub lower_texture: [u8; 8],
    pub middle_texture: [u8; 8],
    pub sector: u16,
}

impl RawSideDef {
    pub fn upper_name(&self) -> String {
        lump_name_str(&self.upper_texture)
    }
    pub fn lower_name(&self) -> String {
        lump_name_str(&self.lower_texture)
    }
    pub fn middle_name(&self) -> String {
        lump_name_str(&self.middle_texture)
    }
}

/// Sector (26 bytes): floor/ceiling info.
#[derive(Debug, Clone)]
pub struct RawSector {
    pub floor_height: i16,
    pub ceiling_height: i16,
    pub floor_texture: [u8; 8],
    pub ceiling_texture: [u8; 8],
    pub light_level: u16,
    pub special: u16,
    pub tag: u16,
}

impl RawSector {
    pub fn floor_name(&self) -> String {
        lump_name_str(&self.floor_texture)
    }
    pub fn ceiling_name(&self) -> String {
        lump_name_str(&self.ceiling_texture)
    }
}

/// Seg (12 bytes): sub-segment of a linedef.
#[derive(Debug, Clone, Copy)]
pub struct RawSeg {
    pub v1: u16,
    pub v2: u16,
    pub angle: i16,
    pub linedef: u16,
    pub direction: u16,
    pub offset: i16,
}

/// SubSector (4 bytes): convex region of the map.
#[derive(Debug, Clone, Copy)]
pub struct RawSubSector {
    pub num_segs: u16,
    pub first_seg: u16,
}

/// BSP Node (28 bytes).
#[derive(Debug, Clone, Copy)]
pub struct RawNode {
    /// Partition line start x.
    pub x: i16,
    /// Partition line start y.
    pub y: i16,
    /// Partition line direction x.
    pub dx: i16,
    /// Partition line direction y.
    pub dy: i16,
    /// Bounding box for right child [top, bottom, left, right].
    pub bbox_right: [i16; 4],
    /// Bounding box for left child [top, bottom, left, right].
    pub bbox_left: [i16; 4],
    /// Right child (high bit set = subsector).
    pub right_child: u16,
    /// Left child (high bit set = subsector).
    pub left_child: u16,
}

/// Bit flag indicating a node child is a subsector.
pub const NF_SUBSECTOR: u16 = 0x8000;

/// Thing (10 bytes): map object placement.
#[derive(Debug, Clone, Copy)]
pub struct RawThing {
    pub x: i16,
    pub y: i16,
    pub angle: u16,
    pub thing_type: u16,
    pub flags: u16,
}

/// Thing flag: appears on skill 1-2.
pub const MTF_EASY: u16 = 1;
/// Thing flag: appears on skill 3.
pub const MTF_NORMAL: u16 = 2;
/// Thing flag: appears on skill 4-5.
pub const MTF_HARD: u16 = 4;
/// Thing flag: deaf (ambush).
pub const MTF_AMBUSH: u16 = 8;
/// Thing flag: multiplayer only.
pub const MTF_MULTI: u16 = 16;

/// Player 1 start thing type.
pub const THING_PLAYER1: u16 = 1;

/// Helper: convert an 8-byte lump name to a trimmed uppercase string.
pub fn lump_name_str(name: &[u8; 8]) -> String {
    let end = name.iter().position(|&b| b == 0).unwrap_or(8);
    String::from_utf8_lossy(&name[..end]).to_uppercase()
}

/// Parse helpers: read little-endian integers from a byte slice.
pub mod parse {
    #[inline]
    pub fn i16_le(data: &[u8], offset: usize) -> i16 {
        i16::from_le_bytes([data[offset], data[offset + 1]])
    }

    #[inline]
    pub fn u16_le(data: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([data[offset], data[offset + 1]])
    }

    #[inline]
    pub fn i32_le(data: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    }

    /// Read an 8-byte fixed-length name field.
    #[inline]
    pub fn name8(data: &[u8], offset: usize) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&data[offset..offset + 8]);
        buf
    }
}
