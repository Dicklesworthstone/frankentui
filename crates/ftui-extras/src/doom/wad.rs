//! WAD file parser for Doom.
//!
//! Parses the WAD directory and provides lump access by name.

use std::collections::HashMap;

use super::wad_types::{
    DirEntry, RawLineDef, RawNode, RawSector, RawSeg, RawSideDef, RawSubSector, RawThing,
    RawVertex, WadHeader,
    parse::{i16_le, i32_le, name8, u16_le},
};

/// A parsed WAD file with lump directory.
#[derive(Debug, Clone)]
pub struct WadFile {
    /// Raw WAD data.
    data: Vec<u8>,
    /// Directory entries.
    pub directory: Vec<DirEntry>,
    /// Map from lump name to first directory index.
    name_index: HashMap<String, usize>,
}

/// Error type for WAD parsing.
#[derive(Debug, Clone)]
pub enum WadError {
    TooSmall,
    BadHeader,
    BadDirectory,
    LumpNotFound(String),
    BadLumpSize(String),
}

impl std::fmt::Display for WadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WadError::TooSmall => write!(f, "WAD data too small"),
            WadError::BadHeader => write!(f, "Invalid WAD header"),
            WadError::BadDirectory => write!(f, "Invalid WAD directory"),
            WadError::LumpNotFound(n) => write!(f, "Lump not found: {n}"),
            WadError::BadLumpSize(n) => write!(f, "Bad lump size for: {n}"),
        }
    }
}

impl WadFile {
    /// Parse a WAD file from raw bytes.
    pub fn parse(data: Vec<u8>) -> Result<Self, WadError> {
        if data.len() < 12 {
            return Err(WadError::TooSmall);
        }

        let header = WadHeader {
            identification: [data[0], data[1], data[2], data[3]],
            num_lumps: i32_le(&data, 4),
            info_table_ofs: i32_le(&data, 8),
        };

        // Validate header
        let id = &header.identification;
        if !(id == b"IWAD" || id == b"PWAD") {
            return Err(WadError::BadHeader);
        }

        let num_lumps = header.num_lumps as usize;
        let dir_offset = header.info_table_ofs as usize;

        if dir_offset + num_lumps * 16 > data.len() {
            return Err(WadError::BadDirectory);
        }

        let mut directory = Vec::with_capacity(num_lumps);
        let mut name_index = HashMap::new();

        for i in 0..num_lumps {
            let off = dir_offset + i * 16;
            let entry = DirEntry {
                filepos: i32_le(&data, off),
                size: i32_le(&data, off + 4),
                name: name8(&data, off + 8),
            };
            let name = entry.name_str();
            name_index.entry(name).or_insert(i);
            directory.push(entry);
        }

        Ok(WadFile {
            data,
            directory,
            name_index,
        })
    }

    /// Find a lump index by name.
    pub fn find_lump(&self, name: &str) -> Option<usize> {
        self.name_index.get(&name.to_uppercase()).copied()
    }

    /// Find a lump index by name, starting search after `start_index`.
    pub fn find_lump_after(&self, name: &str, start_index: usize) -> Option<usize> {
        let upper = name.to_uppercase();
        (start_index + 1..self.directory.len()).find(|&i| self.directory[i].name_str() == upper)
    }

    /// Get raw lump data by directory index.
    pub fn lump_data(&self, index: usize) -> &[u8] {
        let entry = &self.directory[index];
        let start = entry.filepos as usize;
        let end = start + entry.size as usize;
        if end > self.data.len() {
            &[]
        } else {
            &self.data[start..end]
        }
    }

    /// Get raw lump data by name. Returns first match.
    pub fn lump_by_name(&self, name: &str) -> Result<&[u8], WadError> {
        let idx = self
            .find_lump(name)
            .ok_or_else(|| WadError::LumpNotFound(name.to_string()))?;
        Ok(self.lump_data(idx))
    }

    /// Parse VERTEXES lump into vertices.
    pub fn parse_vertices(&self, lump_idx: usize) -> Vec<RawVertex> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 4;
        let mut verts = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 4;
            verts.push(RawVertex {
                x: i16_le(data, off),
                y: i16_le(data, off + 2),
            });
        }
        verts
    }

    /// Parse LINEDEFS lump.
    pub fn parse_linedefs(&self, lump_idx: usize) -> Vec<RawLineDef> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 14;
        let mut lines = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 14;
            lines.push(RawLineDef {
                v1: u16_le(data, off),
                v2: u16_le(data, off + 2),
                flags: u16_le(data, off + 4),
                special: u16_le(data, off + 6),
                tag: u16_le(data, off + 8),
                right_sidedef: u16_le(data, off + 10),
                left_sidedef: u16_le(data, off + 12),
            });
        }
        lines
    }

    /// Parse SIDEDEFS lump.
    pub fn parse_sidedefs(&self, lump_idx: usize) -> Vec<RawSideDef> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 30;
        let mut sides = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 30;
            sides.push(RawSideDef {
                x_offset: i16_le(data, off),
                y_offset: i16_le(data, off + 2),
                upper_texture: name8(data, off + 4),
                lower_texture: name8(data, off + 12),
                middle_texture: name8(data, off + 20),
                sector: u16_le(data, off + 28),
            });
        }
        sides
    }

    /// Parse SECTORS lump.
    pub fn parse_sectors(&self, lump_idx: usize) -> Vec<RawSector> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 26;
        let mut sectors = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 26;
            sectors.push(RawSector {
                floor_height: i16_le(data, off),
                ceiling_height: i16_le(data, off + 2),
                floor_texture: name8(data, off + 4),
                ceiling_texture: name8(data, off + 12),
                light_level: u16_le(data, off + 20),
                special: u16_le(data, off + 22),
                tag: u16_le(data, off + 24),
            });
        }
        sectors
    }

    /// Parse SEGS lump.
    pub fn parse_segs(&self, lump_idx: usize) -> Vec<RawSeg> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 12;
        let mut segs = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 12;
            segs.push(RawSeg {
                v1: u16_le(data, off),
                v2: u16_le(data, off + 2),
                angle: i16_le(data, off + 4),
                linedef: u16_le(data, off + 6),
                direction: u16_le(data, off + 8),
                offset: i16_le(data, off + 10),
            });
        }
        segs
    }

    /// Parse SSECTORS lump.
    pub fn parse_subsectors(&self, lump_idx: usize) -> Vec<RawSubSector> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 4;
        let mut ssectors = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 4;
            ssectors.push(RawSubSector {
                num_segs: u16_le(data, off),
                first_seg: u16_le(data, off + 2),
            });
        }
        ssectors
    }

    /// Parse NODES lump.
    pub fn parse_nodes(&self, lump_idx: usize) -> Vec<RawNode> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 28;
        let mut nodes = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 28;
            nodes.push(RawNode {
                x: i16_le(data, off),
                y: i16_le(data, off + 2),
                dx: i16_le(data, off + 4),
                dy: i16_le(data, off + 6),
                bbox_right: [
                    i16_le(data, off + 8),
                    i16_le(data, off + 10),
                    i16_le(data, off + 12),
                    i16_le(data, off + 14),
                ],
                bbox_left: [
                    i16_le(data, off + 16),
                    i16_le(data, off + 18),
                    i16_le(data, off + 20),
                    i16_le(data, off + 22),
                ],
                right_child: u16_le(data, off + 24),
                left_child: u16_le(data, off + 26),
            });
        }
        nodes
    }

    /// Parse THINGS lump.
    pub fn parse_things(&self, lump_idx: usize) -> Vec<RawThing> {
        let data = self.lump_data(lump_idx);
        let count = data.len() / 10;
        let mut things = Vec::with_capacity(count);
        for i in 0..count {
            let off = i * 10;
            things.push(RawThing {
                x: i16_le(data, off),
                y: i16_le(data, off + 2),
                angle: u16_le(data, off + 4),
                thing_type: u16_le(data, off + 6),
                flags: u16_le(data, off + 8),
            });
        }
        things
    }

    /// Parse PLAYPAL lump (14 palettes × 256 colors × 3 bytes RGB).
    pub fn parse_playpal(&self) -> Result<Vec<[u8; 3]>, WadError> {
        let data = self.lump_by_name("PLAYPAL")?;
        if data.len() < 768 {
            return Err(WadError::BadLumpSize("PLAYPAL".into()));
        }
        // Just use the first palette
        let mut palette = Vec::with_capacity(256);
        for i in 0..256 {
            let off = i * 3;
            palette.push([data[off], data[off + 1], data[off + 2]]);
        }
        Ok(palette)
    }

    /// Parse COLORMAP lump (34 maps × 256 bytes).
    pub fn parse_colormap(&self) -> Result<Vec<Vec<u8>>, WadError> {
        let data = self.lump_by_name("COLORMAP")?;
        if data.len() < 34 * 256 {
            return Err(WadError::BadLumpSize("COLORMAP".into()));
        }
        let mut maps = Vec::with_capacity(34);
        for m in 0..34 {
            let off = m * 256;
            maps.push(data[off..off + 256].to_vec());
        }
        Ok(maps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_minimal_wad() -> Vec<u8> {
        // Create a minimal valid WAD with 0 lumps
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD"); // identification
        data.extend_from_slice(&0i32.to_le_bytes()); // num_lumps = 0
        data.extend_from_slice(&12i32.to_le_bytes()); // info_table_ofs = 12 (right after header)
        data
    }

    #[test]
    fn parse_empty_wad() {
        let data = make_minimal_wad();
        let wad = WadFile::parse(data).unwrap();
        assert_eq!(wad.directory.len(), 0);
    }

    #[test]
    fn reject_too_small() {
        assert!(WadFile::parse(vec![0; 4]).is_err());
    }

    #[test]
    fn reject_bad_header() {
        let mut data = make_minimal_wad();
        data[0] = b'X';
        assert!(WadFile::parse(data).is_err());
    }

    #[test]
    fn parse_wad_with_lump() {
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes()); // 1 lump
        data.extend_from_slice(&16i32.to_le_bytes()); // dir at byte 16

        // Lump data at offset 12 (4 bytes)
        data.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        // Directory entry at offset 16
        data.extend_from_slice(&12i32.to_le_bytes()); // filepos
        data.extend_from_slice(&4i32.to_le_bytes()); // size
        data.extend_from_slice(b"TESTLUMP"); // name

        let wad = WadFile::parse(data).unwrap();
        assert_eq!(wad.directory.len(), 1);
        assert_eq!(wad.directory[0].name_str(), "TESTLUMP");
        assert_eq!(wad.lump_data(0), &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(wad.find_lump("TESTLUMP").is_some());
    }

    #[test]
    fn pwad_header_accepted() {
        let mut data = Vec::new();
        data.extend_from_slice(b"PWAD");
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&12i32.to_le_bytes());
        let wad = WadFile::parse(data).unwrap();
        assert_eq!(wad.directory.len(), 0);
    }

    #[test]
    fn find_lump_case_insensitive() {
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&16i32.to_le_bytes());
        data.extend_from_slice(&[0; 4]); // lump data placeholder
        data.extend_from_slice(&12i32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(b"THINGS\0\0");

        let wad = WadFile::parse(data).unwrap();
        // Search with lowercase should find uppercase lump
        assert!(wad.find_lump("things").is_some());
        assert!(wad.find_lump("THINGS").is_some());
    }

    #[test]
    fn find_lump_not_found() {
        let data = make_minimal_wad();
        let wad = WadFile::parse(data).unwrap();
        assert!(wad.find_lump("MISSING").is_none());
    }

    #[test]
    fn lump_by_name_error() {
        let data = make_minimal_wad();
        let wad = WadFile::parse(data).unwrap();
        let err = wad.lump_by_name("MISSING");
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("MISSING"));
    }

    fn make_two_lump_wad() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&2i32.to_le_bytes()); // 2 lumps
        data.extend_from_slice(&20i32.to_le_bytes()); // dir at byte 20

        // Lump 0 data at offset 12 (4 bytes)
        data.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        // Lump 1 data at offset 16 (4 bytes)
        data.extend_from_slice(&[0x05, 0x06, 0x07, 0x08]);

        // Directory entry 0 at offset 20
        data.extend_from_slice(&12i32.to_le_bytes()); // filepos
        data.extend_from_slice(&4i32.to_le_bytes()); // size
        data.extend_from_slice(b"MAP01\0\0\0"); // name

        // Directory entry 1 at offset 36
        data.extend_from_slice(&16i32.to_le_bytes()); // filepos
        data.extend_from_slice(&4i32.to_le_bytes()); // size
        data.extend_from_slice(b"THINGS\0\0"); // name

        data
    }

    #[test]
    fn find_lump_after_skips_earlier() {
        let data = make_two_lump_wad();
        let wad = WadFile::parse(data).unwrap();
        // find_lump_after(name, start_index) searches after start_index
        let idx = wad.find_lump_after("THINGS", 0);
        assert_eq!(idx, Some(1));
        // Searching after index 1 should find nothing
        let idx = wad.find_lump_after("THINGS", 1);
        assert_eq!(idx, None);
    }

    #[test]
    fn lump_data_out_of_range_returns_empty() {
        // Create a WAD with a lump that has filepos beyond data
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&12i32.to_le_bytes()); // dir at byte 12

        // Directory entry at offset 12
        data.extend_from_slice(&9999i32.to_le_bytes()); // filepos way beyond data
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(b"BADLUMP\0");

        let wad = WadFile::parse(data).unwrap();
        assert!(wad.lump_data(0).is_empty());
    }

    #[test]
    fn parse_vertices_from_lump() {
        // Build a WAD with VERTEXES lump containing 2 vertices
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&20i32.to_le_bytes()); // dir at byte 20

        // Vertex data at offset 12 (2 verts × 4 bytes = 8 bytes)
        data.extend_from_slice(&100i16.to_le_bytes()); // v0.x
        data.extend_from_slice(&200i16.to_le_bytes()); // v0.y
        data.extend_from_slice(&(-50i16).to_le_bytes()); // v1.x
        data.extend_from_slice(&300i16.to_le_bytes()); // v1.y

        // Directory entry at offset 20
        data.extend_from_slice(&12i32.to_le_bytes()); // filepos
        data.extend_from_slice(&8i32.to_le_bytes()); // size
        data.extend_from_slice(b"VERTEXES"); // name

        let wad = WadFile::parse(data).unwrap();
        let verts = wad.parse_vertices(0);
        assert_eq!(verts.len(), 2);
        assert_eq!(verts[0].x, 100);
        assert_eq!(verts[0].y, 200);
        assert_eq!(verts[1].x, -50);
        assert_eq!(verts[1].y, 300);
    }

    #[test]
    fn bad_directory_offset() {
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&9999i32.to_le_bytes()); // dir offset way beyond data
        assert!(WadFile::parse(data).is_err());
    }

    #[test]
    fn wad_error_display() {
        assert_eq!(format!("{}", WadError::TooSmall), "WAD data too small");
        assert_eq!(format!("{}", WadError::BadHeader), "Invalid WAD header");
        assert_eq!(
            format!("{}", WadError::BadDirectory),
            "Invalid WAD directory"
        );
        assert!(format!("{}", WadError::LumpNotFound("X".into())).contains("X"));
        assert!(format!("{}", WadError::BadLumpSize("Y".into())).contains("Y"));
    }

    // --- Helper: build a WAD with a single lump of arbitrary data ---

    fn make_wad_with_lump(lump_name: &[u8; 8], lump_data: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes()); // 1 lump
        let dir_offset = 12 + lump_data.len() as i32;
        data.extend_from_slice(&dir_offset.to_le_bytes()); // dir after header + lump data

        // Lump data at offset 12
        data.extend_from_slice(lump_data);

        // Directory entry
        data.extend_from_slice(&12i32.to_le_bytes()); // filepos
        data.extend_from_slice(&(lump_data.len() as i32).to_le_bytes()); // size
        data.extend_from_slice(lump_name); // name
        data
    }

    // --- parse_linedefs ---

    #[test]
    fn parse_linedefs_single() {
        // One linedef = 14 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&1u16.to_le_bytes()); // v1
        lump.extend_from_slice(&2u16.to_le_bytes()); // v2
        lump.extend_from_slice(&0x0001u16.to_le_bytes()); // flags
        lump.extend_from_slice(&0u16.to_le_bytes()); // special
        lump.extend_from_slice(&0u16.to_le_bytes()); // tag
        lump.extend_from_slice(&0u16.to_le_bytes()); // right_sidedef
        lump.extend_from_slice(&0xFFFFu16.to_le_bytes()); // left_sidedef

        let wad_data = make_wad_with_lump(b"LINEDEFS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let linedefs = wad.parse_linedefs(0);
        assert_eq!(linedefs.len(), 1);
        assert_eq!(linedefs[0].v1, 1);
        assert_eq!(linedefs[0].v2, 2);
        assert_eq!(linedefs[0].flags, 1);
        assert_eq!(linedefs[0].left_sidedef, 0xFFFF);
    }

    // --- parse_sidedefs ---

    #[test]
    fn parse_sidedefs_single() {
        // One sidedef = 30 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&10i16.to_le_bytes()); // x_offset
        lump.extend_from_slice(&20i16.to_le_bytes()); // y_offset
        lump.extend_from_slice(b"UPPER\0\0\0"); // upper_texture
        lump.extend_from_slice(b"LOWER\0\0\0"); // lower_texture
        lump.extend_from_slice(b"MIDDLE\0\0"); // middle_texture
        lump.extend_from_slice(&3u16.to_le_bytes()); // sector

        let wad_data = make_wad_with_lump(b"SIDEDEFS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let sides = wad.parse_sidedefs(0);
        assert_eq!(sides.len(), 1);
        assert_eq!(sides[0].x_offset, 10);
        assert_eq!(sides[0].y_offset, 20);
        assert_eq!(sides[0].sector, 3);
    }

    // --- parse_sectors ---

    #[test]
    fn parse_sectors_single() {
        // One sector = 26 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&0i16.to_le_bytes()); // floor_height
        lump.extend_from_slice(&128i16.to_le_bytes()); // ceiling_height
        lump.extend_from_slice(b"FLOOR4_8"); // floor_texture
        lump.extend_from_slice(b"CEIL3_5\0"); // ceiling_texture
        lump.extend_from_slice(&200u16.to_le_bytes()); // light_level
        lump.extend_from_slice(&0u16.to_le_bytes()); // special
        lump.extend_from_slice(&0u16.to_le_bytes()); // tag

        let wad_data = make_wad_with_lump(b"SECTORS\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let sectors = wad.parse_sectors(0);
        assert_eq!(sectors.len(), 1);
        assert_eq!(sectors[0].floor_height, 0);
        assert_eq!(sectors[0].ceiling_height, 128);
        assert_eq!(sectors[0].light_level, 200);
    }

    // --- parse_segs ---

    #[test]
    fn parse_segs_single() {
        // One seg = 12 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&0u16.to_le_bytes()); // v1
        lump.extend_from_slice(&1u16.to_le_bytes()); // v2
        lump.extend_from_slice(&(-16384i16).to_le_bytes()); // angle
        lump.extend_from_slice(&5u16.to_le_bytes()); // linedef
        lump.extend_from_slice(&0u16.to_le_bytes()); // direction
        lump.extend_from_slice(&0i16.to_le_bytes()); // offset

        let wad_data = make_wad_with_lump(b"SEGS\0\0\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let segs = wad.parse_segs(0);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].v1, 0);
        assert_eq!(segs[0].v2, 1);
        assert_eq!(segs[0].angle, -16384);
        assert_eq!(segs[0].linedef, 5);
    }

    // --- parse_subsectors ---

    #[test]
    fn parse_subsectors_single() {
        // One ssector = 4 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&4u16.to_le_bytes()); // num_segs
        lump.extend_from_slice(&0u16.to_le_bytes()); // first_seg

        let wad_data = make_wad_with_lump(b"SSECTORS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let ssectors = wad.parse_subsectors(0);
        assert_eq!(ssectors.len(), 1);
        assert_eq!(ssectors[0].num_segs, 4);
        assert_eq!(ssectors[0].first_seg, 0);
    }

    // --- parse_nodes ---

    #[test]
    fn parse_nodes_single() {
        // One node = 28 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&100i16.to_le_bytes()); // x
        lump.extend_from_slice(&200i16.to_le_bytes()); // y
        lump.extend_from_slice(&0i16.to_le_bytes()); // dx
        lump.extend_from_slice(&1i16.to_le_bytes()); // dy
        for _ in 0..4 {
            lump.extend_from_slice(&0i16.to_le_bytes()); // bbox_right
        }
        for _ in 0..4 {
            lump.extend_from_slice(&0i16.to_le_bytes()); // bbox_left
        }
        lump.extend_from_slice(&0u16.to_le_bytes()); // right_child
        lump.extend_from_slice(&(0x8000u16 | 1).to_le_bytes()); // left_child (subsector flag)

        let wad_data = make_wad_with_lump(b"NODES\0\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let nodes = wad.parse_nodes(0);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].x, 100);
        assert_eq!(nodes[0].y, 200);
        assert_eq!(nodes[0].dx, 0);
        assert_eq!(nodes[0].dy, 1);
        assert_eq!(nodes[0].left_child, 0x8000 | 1);
    }

    // --- parse_things ---

    #[test]
    fn parse_things_single() {
        // One thing = 10 bytes
        let mut lump = Vec::new();
        lump.extend_from_slice(&(-100i16).to_le_bytes()); // x
        lump.extend_from_slice(&50i16.to_le_bytes()); // y
        lump.extend_from_slice(&90u16.to_le_bytes()); // angle
        lump.extend_from_slice(&1u16.to_le_bytes()); // thing_type (player 1)
        lump.extend_from_slice(&0x0007u16.to_le_bytes()); // flags

        let wad_data = make_wad_with_lump(b"THINGS\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let things = wad.parse_things(0);
        assert_eq!(things.len(), 1);
        assert_eq!(things[0].x, -100);
        assert_eq!(things[0].y, 50);
        assert_eq!(things[0].angle, 90);
        assert_eq!(things[0].thing_type, 1);
        assert_eq!(things[0].flags, 7);
    }

    // --- parse_playpal ---

    #[test]
    fn parse_playpal_valid() {
        // Minimal PLAYPAL: 768 bytes (256 colors × 3 bytes)
        let mut lump = vec![0u8; 768];
        lump[0] = 255; // first color R
        lump[1] = 128; // first color G
        lump[2] = 64; // first color B
        lump[765] = 10; // last color R
        lump[766] = 20; // last color G
        lump[767] = 30; // last color B

        let wad_data = make_wad_with_lump(b"PLAYPAL\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let palette = wad.parse_playpal().unwrap();
        assert_eq!(palette.len(), 256);
        assert_eq!(palette[0], [255, 128, 64]);
        assert_eq!(palette[255], [10, 20, 30]);
    }

    #[test]
    fn parse_playpal_too_small() {
        let lump = vec![0u8; 100]; // way too small
        let wad_data = make_wad_with_lump(b"PLAYPAL\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_playpal().is_err());
    }

    #[test]
    fn parse_playpal_missing_lump() {
        let data = make_minimal_wad();
        let wad = WadFile::parse(data).unwrap();
        assert!(wad.parse_playpal().is_err());
    }

    // --- parse_colormap ---

    #[test]
    fn parse_colormap_valid() {
        // 34 maps × 256 bytes = 8704 bytes
        let mut lump = vec![0u8; 34 * 256];
        lump[0] = 42; // first map, first entry
        lump[256] = 99; // second map, first entry

        let wad_data = make_wad_with_lump(b"COLORMAP", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let maps = wad.parse_colormap().unwrap();
        assert_eq!(maps.len(), 34);
        assert_eq!(maps[0].len(), 256);
        assert_eq!(maps[0][0], 42);
        assert_eq!(maps[1][0], 99);
    }

    #[test]
    fn parse_colormap_too_small() {
        let lump = vec![0u8; 100];
        let wad_data = make_wad_with_lump(b"COLORMAP", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_colormap().is_err());
    }

    // --- Multiple lumps with same name ---

    #[test]
    fn duplicate_lump_names_uses_first() {
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&2i32.to_le_bytes()); // 2 lumps
        let dir_offset: i32 = 12 + 8; // after 2 × 4 bytes of lump data
        data.extend_from_slice(&dir_offset.to_le_bytes());

        // Lump 0 data
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        // Lump 1 data
        data.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);

        // Dir entry 0
        data.extend_from_slice(&12i32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(b"DUPE\0\0\0\0");

        // Dir entry 1 (same name!)
        data.extend_from_slice(&16i32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(b"DUPE\0\0\0\0");

        let wad = WadFile::parse(data).unwrap();
        assert_eq!(wad.find_lump("DUPE"), Some(0));
        assert_eq!(wad.lump_data(0), &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    // --- Empty lump data ---

    #[test]
    fn parse_vertices_empty_lump() {
        let wad_data = make_wad_with_lump(b"VERTEXES", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        let verts = wad.parse_vertices(0);
        assert!(verts.is_empty());
    }

    #[test]
    fn parse_linedefs_empty_lump() {
        let wad_data = make_wad_with_lump(b"LINEDEFS", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        let linedefs = wad.parse_linedefs(0);
        assert!(linedefs.is_empty());
    }

    #[test]
    fn parse_things_empty_lump() {
        let wad_data = make_wad_with_lump(b"THINGS\0\0", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        let things = wad.parse_things(0);
        assert!(things.is_empty());
    }

    // --- Edge cases ---

    #[test]
    fn exact_minimum_size_parses() {
        // 12 bytes is the minimum (header only, 0 lumps)
        let data = vec![
            b'I', b'W', b'A', b'D', // identification
            0, 0, 0, 0, // num_lumps = 0
            12, 0, 0, 0, // info_table_ofs = 12
        ];
        let wad = WadFile::parse(data).unwrap();
        assert!(wad.directory.is_empty());
    }

    #[test]
    fn eleven_bytes_too_small() {
        assert!(WadFile::parse(vec![0; 11]).is_err());
    }

    #[test]
    fn parse_multiple_vertices() {
        let mut lump = Vec::new();
        for i in 0..10i16 {
            lump.extend_from_slice(&(i * 10).to_le_bytes()); // x
            lump.extend_from_slice(&(i * 20).to_le_bytes()); // y
        }
        let wad_data = make_wad_with_lump(b"VERTEXES", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let verts = wad.parse_vertices(0);
        assert_eq!(verts.len(), 10);
        assert_eq!(verts[5].x, 50);
        assert_eq!(verts[5].y, 100);
    }

    #[test]
    fn find_lump_after_from_start() {
        let data = make_two_lump_wad();
        let wad = WadFile::parse(data).unwrap();
        // Search for MAP01 starting from 0 → not found (MAP01 is at index 0, search starts after 0)
        let idx = wad.find_lump_after("MAP01", 0);
        assert_eq!(idx, None);
    }

    #[test]
    fn parse_colormap_missing_lump() {
        let data = make_minimal_wad();
        let wad = WadFile::parse(data).unwrap();
        assert!(wad.parse_colormap().is_err());
    }

    #[test]
    fn parse_multiple_linedefs() {
        let mut lump = Vec::new();
        for i in 0..5u16 {
            lump.extend_from_slice(&i.to_le_bytes()); // v1
            lump.extend_from_slice(&(i + 1).to_le_bytes()); // v2
            lump.extend_from_slice(&0u16.to_le_bytes()); // flags
            lump.extend_from_slice(&0u16.to_le_bytes()); // special
            lump.extend_from_slice(&0u16.to_le_bytes()); // tag
            lump.extend_from_slice(&0u16.to_le_bytes()); // right_sidedef
            lump.extend_from_slice(&0xFFFFu16.to_le_bytes()); // left_sidedef
        }
        let wad_data = make_wad_with_lump(b"LINEDEFS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let linedefs = wad.parse_linedefs(0);
        assert_eq!(linedefs.len(), 5);
        assert_eq!(linedefs[3].v1, 3);
        assert_eq!(linedefs[3].v2, 4);
    }

    #[test]
    fn parse_multiple_sectors() {
        let mut lump = Vec::new();
        for i in 0..3i16 {
            lump.extend_from_slice(&(i * 10).to_le_bytes()); // floor_height
            lump.extend_from_slice(&128i16.to_le_bytes()); // ceiling_height
            lump.extend_from_slice(b"FLOOR4_8"); // floor_texture
            lump.extend_from_slice(b"CEIL3_5\0"); // ceiling_texture
            lump.extend_from_slice(&160u16.to_le_bytes()); // light_level
            lump.extend_from_slice(&0u16.to_le_bytes()); // special
            lump.extend_from_slice(&0u16.to_le_bytes()); // tag
        }
        let wad_data = make_wad_with_lump(b"SECTORS\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let sectors = wad.parse_sectors(0);
        assert_eq!(sectors.len(), 3);
        assert_eq!(sectors[2].floor_height, 20);
    }

    #[test]
    fn parse_multiple_segs() {
        let mut lump = Vec::new();
        for i in 0..4u16 {
            lump.extend_from_slice(&i.to_le_bytes()); // v1
            lump.extend_from_slice(&(i + 1).to_le_bytes()); // v2
            lump.extend_from_slice(&0i16.to_le_bytes()); // angle
            lump.extend_from_slice(&i.to_le_bytes()); // linedef
            lump.extend_from_slice(&0u16.to_le_bytes()); // direction
            lump.extend_from_slice(&0i16.to_le_bytes()); // offset
        }
        let wad_data = make_wad_with_lump(b"SEGS\0\0\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let segs = wad.parse_segs(0);
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[2].v1, 2);
        assert_eq!(segs[2].linedef, 2);
    }

    #[test]
    fn parse_multiple_subsectors() {
        let mut lump = Vec::new();
        for i in 0..3u16 {
            lump.extend_from_slice(&(i + 2).to_le_bytes()); // num_segs
            lump.extend_from_slice(&(i * 3).to_le_bytes()); // first_seg
        }
        let wad_data = make_wad_with_lump(b"SSECTORS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let ssectors = wad.parse_subsectors(0);
        assert_eq!(ssectors.len(), 3);
        assert_eq!(ssectors[1].num_segs, 3);
        assert_eq!(ssectors[1].first_seg, 3);
    }

    #[test]
    fn parse_multiple_nodes() {
        let mut lump = Vec::new();
        for i in 0..2i16 {
            lump.extend_from_slice(&(i * 100).to_le_bytes()); // x
            lump.extend_from_slice(&(i * 200).to_le_bytes()); // y
            lump.extend_from_slice(&1i16.to_le_bytes()); // dx
            lump.extend_from_slice(&0i16.to_le_bytes()); // dy
            for _ in 0..8 {
                lump.extend_from_slice(&0i16.to_le_bytes()); // bbox entries
            }
            lump.extend_from_slice(&0u16.to_le_bytes()); // right_child
            lump.extend_from_slice(&0u16.to_le_bytes()); // left_child
        }
        let wad_data = make_wad_with_lump(b"NODES\0\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let nodes = wad.parse_nodes(0);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].x, 100);
        assert_eq!(nodes[1].y, 200);
    }

    #[test]
    fn parse_multiple_things() {
        let mut lump = Vec::new();
        for i in 0..3i16 {
            lump.extend_from_slice(&(i * 50).to_le_bytes()); // x
            lump.extend_from_slice(&(i * 25).to_le_bytes()); // y
            lump.extend_from_slice(&((i as u16) * 90).to_le_bytes()); // angle
            lump.extend_from_slice(&1u16.to_le_bytes()); // thing_type
            lump.extend_from_slice(&7u16.to_le_bytes()); // flags
        }
        let wad_data = make_wad_with_lump(b"THINGS\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let things = wad.parse_things(0);
        assert_eq!(things.len(), 3);
        assert_eq!(things[2].x, 100);
        assert_eq!(things[2].y, 50);
        assert_eq!(things[2].angle, 180);
    }

    #[test]
    fn parse_multiple_sidedefs() {
        let mut lump = Vec::new();
        for i in 0..2i16 {
            lump.extend_from_slice(&(i * 5).to_le_bytes()); // x_offset
            lump.extend_from_slice(&(i * 10).to_le_bytes()); // y_offset
            lump.extend_from_slice(b"UPPER\0\0\0"); // upper_texture
            lump.extend_from_slice(b"LOWER\0\0\0"); // lower_texture
            lump.extend_from_slice(b"MIDDLE\0\0"); // middle_texture
            lump.extend_from_slice(&((i as u16) + 1).to_le_bytes()); // sector
        }
        let wad_data = make_wad_with_lump(b"SIDEDEFS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let sides = wad.parse_sidedefs(0);
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[1].x_offset, 5);
        assert_eq!(sides[1].y_offset, 10);
        assert_eq!(sides[1].sector, 2);
    }

    #[test]
    fn parse_segs_empty_lump() {
        let wad_data = make_wad_with_lump(b"SEGS\0\0\0\0", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_segs(0).is_empty());
    }

    #[test]
    fn parse_subsectors_empty_lump() {
        let wad_data = make_wad_with_lump(b"SSECTORS", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_subsectors(0).is_empty());
    }

    #[test]
    fn parse_nodes_empty_lump() {
        let wad_data = make_wad_with_lump(b"NODES\0\0\0", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_nodes(0).is_empty());
    }

    #[test]
    fn parse_sectors_empty_lump() {
        let wad_data = make_wad_with_lump(b"SECTORS\0", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_sectors(0).is_empty());
    }

    #[test]
    fn parse_sidedefs_empty_lump() {
        let wad_data = make_wad_with_lump(b"SIDEDEFS", &[]);
        let wad = WadFile::parse(wad_data).unwrap();
        assert!(wad.parse_sidedefs(0).is_empty());
    }

    #[test]
    fn find_lump_after_case_insensitive() {
        let data = make_two_lump_wad();
        let wad = WadFile::parse(data).unwrap();
        // find_lump_after should be case-insensitive
        let idx = wad.find_lump_after("things", 0);
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn find_lump_after_beyond_end() {
        let data = make_two_lump_wad();
        let wad = WadFile::parse(data).unwrap();
        // Searching beyond the last index returns None
        let idx = wad.find_lump_after("THINGS", 100);
        assert_eq!(idx, None);
    }

    #[test]
    fn lump_data_with_zero_size() {
        // Create a WAD with a zero-size lump
        let mut data = Vec::new();
        data.extend_from_slice(b"IWAD");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&12i32.to_le_bytes()); // dir at 12

        // Directory entry with size 0
        data.extend_from_slice(&12i32.to_le_bytes()); // filepos (points to dir itself, but size=0)
        data.extend_from_slice(&0i32.to_le_bytes()); // size = 0
        data.extend_from_slice(b"EMPTY\0\0\0");

        let wad = WadFile::parse(data).unwrap();
        assert!(wad.lump_data(0).is_empty());
    }

    #[test]
    fn parse_playpal_larger_than_minimum() {
        // PLAYPAL can have multiple palettes (14 × 768 bytes)
        // but we only use the first. Verify extra data doesn't cause issues.
        let lump = vec![42u8; 768 * 14];
        let wad_data = make_wad_with_lump(b"PLAYPAL\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let palette = wad.parse_playpal().unwrap();
        assert_eq!(palette.len(), 256);
        assert_eq!(palette[0], [42, 42, 42]);
    }

    #[test]
    fn parse_partial_linedef_data_truncates() {
        // 13 bytes = less than one full linedef (14 bytes), should produce 0
        let lump = vec![0u8; 13];
        let wad_data = make_wad_with_lump(b"LINEDEFS", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let linedefs = wad.parse_linedefs(0);
        assert!(linedefs.is_empty());
    }

    #[test]
    fn parse_partial_thing_data_truncates() {
        // 9 bytes = less than one full thing (10 bytes), should produce 0
        let lump = vec![0u8; 9];
        let wad_data = make_wad_with_lump(b"THINGS\0\0", &lump);
        let wad = WadFile::parse(wad_data).unwrap();
        let things = wad.parse_things(0);
        assert!(things.is_empty());
    }

    #[test]
    fn dir_entry_name_str_trims_nulls() {
        let entry = DirEntry {
            filepos: 0,
            size: 0,
            name: *b"HI\0\0\0\0\0\0",
        };
        assert_eq!(entry.name_str(), "HI");
    }

    #[test]
    fn dir_entry_name_str_full_8_chars() {
        let entry = DirEntry {
            filepos: 0,
            size: 0,
            name: *b"LONGNAME",
        };
        assert_eq!(entry.name_str(), "LONGNAME");
    }
}
