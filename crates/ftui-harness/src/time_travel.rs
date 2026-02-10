#![forbid(unsafe_code)]

//! Time-travel debugging with frame snapshots.
//!
//! Records compressed frame snapshots during development, enabling "rewind"
//! to inspect past visual states. Frames are delta-encoded for efficient
//! memory usage.
//!
//! # Quick Start
//!
//! ```
//! use ftui_harness::time_travel::{TimeTravel, FrameMetadata};
//! use ftui_render::buffer::Buffer;
//! use ftui_render::cell::Cell;
//! use std::time::Duration;
//!
//! let mut tt = TimeTravel::new(100);
//!
//! // Record frames
//! let mut buf = Buffer::new(10, 5);
//! buf.set(0, 0, Cell::from_char('A'));
//! tt.record(&buf, FrameMetadata::new(0, Duration::from_millis(2)));
//!
//! buf.set(1, 0, Cell::from_char('B'));
//! tt.record(&buf, FrameMetadata::new(1, Duration::from_millis(3)));
//!
//! // Rewind
//! let frame = tt.rewind(0).unwrap(); // current frame
//! assert_eq!(frame.get(1, 0).unwrap().content.as_char(), Some('B'));
//!
//! let prev = tt.rewind(1).unwrap(); // one step back
//! assert!(prev.get(1, 0).unwrap().is_empty());
//! ```
//!
//! # Export
//!
//! ```ignore
//! tt.export(Path::new("session.fttr"))?;
//! let loaded = TimeTravel::import(Path::new("session.fttr"))?;
//! ```

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;

// ============================================================================
// Cell Change
// ============================================================================

/// A single changed cell in a frame delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellChange {
    pub x: u16,
    pub y: u16,
    pub cell: Cell,
}

impl CellChange {
    /// Serialized size: x(2) + y(2) + cell content(4) + fg(4) + bg(4) + attrs(4) = 20 bytes.
    #[cfg(test)]
    const SERIALIZED_SIZE: usize = 20;

    fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.x.to_le_bytes())?;
        w.write_all(&self.y.to_le_bytes())?;
        w.write_all(&self.cell.content.raw().to_le_bytes())?;
        w.write_all(&self.cell.fg.0.to_le_bytes())?;
        w.write_all(&self.cell.bg.0.to_le_bytes())?;
        // CellAttrs: reconstruct from flags + link_id
        let flags_byte = self.cell.attrs.flags().bits();
        let link_id = self.cell.attrs.link_id();
        let attrs_raw = ((flags_byte as u32) << 24) | (link_id & 0x00FF_FFFF);
        w.write_all(&attrs_raw.to_le_bytes())
    }

    fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut buf2 = [0u8; 2];
        let mut buf4 = [0u8; 4];

        r.read_exact(&mut buf2)?;
        let x = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf2)?;
        let y = u16::from_le_bytes(buf2);

        r.read_exact(&mut buf4)?;
        let content_raw = u32::from_le_bytes(buf4);
        r.read_exact(&mut buf4)?;
        let fg_raw = u32::from_le_bytes(buf4);
        r.read_exact(&mut buf4)?;
        let bg_raw = u32::from_le_bytes(buf4);
        r.read_exact(&mut buf4)?;
        let attrs_raw = u32::from_le_bytes(buf4);

        use ftui_render::cell::{CellAttrs, CellContent, GraphemeId, PackedRgba, StyleFlags};

        // Reconstruct CellContent from raw u32.
        let content = if content_raw & 0x8000_0000 != 0 {
            CellContent::from_grapheme(GraphemeId::from_raw(content_raw & !0x8000_0000))
        } else if content_raw == CellContent::EMPTY.raw() {
            CellContent::EMPTY
        } else if content_raw == CellContent::CONTINUATION.raw() {
            CellContent::CONTINUATION
        } else {
            char::from_u32(content_raw)
                .map(CellContent::from_char)
                .unwrap_or(CellContent::EMPTY)
        };
        let fg = PackedRgba(fg_raw);
        let bg = PackedRgba(bg_raw);
        let flags = StyleFlags::from_bits_truncate((attrs_raw >> 24) as u8);
        let link_id = attrs_raw & 0x00FF_FFFF;
        let attrs = CellAttrs::new(flags, link_id);

        Ok(CellChange {
            x,
            y,
            cell: Cell {
                content,
                fg,
                bg,
                attrs,
            },
        })
    }
}

// ============================================================================
// Compressed Frame
// ============================================================================

/// A delta-encoded frame snapshot.
///
/// Stores only the cells that changed from the previous frame.
/// The first frame in a recording is always a full snapshot
/// (all non-empty cells are stored as changes from an empty buffer).
#[derive(Debug, Clone)]
pub struct CompressedFrame {
    /// Buffer dimensions (needed to reconstruct).
    width: u16,
    height: u16,
    /// Changed cells (sparse delta).
    changes: Vec<CellChange>,
    /// Cursor position at time of snapshot.
    cursor: Option<(u16, u16)>,
}

impl CompressedFrame {
    /// Create a full snapshot (delta from empty buffer).
    pub fn full(buf: &Buffer) -> Self {
        let mut changes = Vec::new();
        let default = Cell::default();

        for y in 0..buf.height() {
            for x in 0..buf.width() {
                let cell = buf.get(x, y).unwrap();
                if !cell.bits_eq(&default) {
                    changes.push(CellChange { x, y, cell: *cell });
                }
            }
        }

        Self {
            width: buf.width(),
            height: buf.height(),
            changes,
            cursor: None,
        }
    }

    /// Create a delta-encoded snapshot from the previous buffer state.
    pub fn delta(current: &Buffer, previous: &Buffer) -> Self {
        debug_assert_eq!(current.width(), previous.width());
        debug_assert_eq!(current.height(), previous.height());

        let mut changes = Vec::new();

        for y in 0..current.height() {
            for x in 0..current.width() {
                let curr = current.get(x, y).unwrap();
                let prev = previous.get(x, y).unwrap();
                if !curr.bits_eq(prev) {
                    changes.push(CellChange { x, y, cell: *curr });
                }
            }
        }

        Self {
            width: current.width(),
            height: current.height(),
            changes,
            cursor: None,
        }
    }

    /// Apply this frame's changes to a buffer.
    ///
    /// The buffer must have matching dimensions.
    pub fn apply_to(&self, buf: &mut Buffer) {
        debug_assert_eq!(buf.width(), self.width);
        debug_assert_eq!(buf.height(), self.height);

        for change in &self.changes {
            buf.set_raw(change.x, change.y, change.cell);
        }
    }

    /// Number of changed cells in this frame.
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    /// Estimated memory size in bytes.
    pub fn memory_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.changes.len() * std::mem::size_of::<CellChange>()
    }

    /// Set cursor position for this frame.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Option<(u16, u16)>) -> Self {
        self.cursor = cursor;
        self
    }
}

// ============================================================================
// Frame Metadata
// ============================================================================

/// Metadata attached to each recorded frame.
#[derive(Debug, Clone)]
pub struct FrameMetadata {
    /// Frame sequence number.
    pub frame_number: u64,
    /// Time taken to render this frame.
    pub render_time: Duration,
    /// Number of events that triggered this frame.
    pub event_count: u32,
    /// Optional model state hash for identifying state.
    pub model_hash: Option<u64>,
}

impl FrameMetadata {
    /// Create metadata with required fields.
    pub fn new(frame_number: u64, render_time: Duration) -> Self {
        Self {
            frame_number,
            render_time,
            event_count: 0,
            model_hash: None,
        }
    }

    /// Set event count.
    #[must_use]
    pub fn with_events(mut self, count: u32) -> Self {
        self.event_count = count;
        self
    }

    /// Set model hash.
    #[must_use]
    pub fn with_model_hash(mut self, hash: u64) -> Self {
        self.model_hash = Some(hash);
        self
    }
}

// ============================================================================
// TimeTravel
// ============================================================================

/// Time-travel recording session.
///
/// Records frame snapshots in a circular buffer, enabling rewind to
/// inspect past visual states. Frames are delta-encoded from the
/// previous frame for efficient memory usage.
///
/// # Memory Budget
///
/// With default capacity (1000 frames) and typical delta size (~100 bytes
/// for 5% cell change), total memory is approximately 100KB.
#[derive(Debug)]
pub struct TimeTravel {
    /// Circular buffer of compressed frame snapshots.
    snapshots: VecDeque<CompressedFrame>,
    /// Metadata for each frame.
    metadata: VecDeque<FrameMetadata>,
    /// Maximum number of snapshots to retain.
    capacity: usize,
    /// Running frame counter.
    frame_counter: u64,
    /// Whether recording is active.
    recording: bool,
    /// The last buffer state (for computing deltas).
    last_buffer: Option<Buffer>,
}

impl TimeTravel {
    /// Create a new recorder with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if capacity is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "TimeTravel capacity must be > 0");
        Self {
            snapshots: VecDeque::with_capacity(capacity),
            metadata: VecDeque::with_capacity(capacity),
            capacity,
            frame_counter: 0,
            recording: true,
            last_buffer: None,
        }
    }

    /// Record a frame snapshot.
    ///
    /// If recording is paused, this is a no-op. The frame is delta-encoded
    /// from the previous recorded frame, or stored as a full snapshot if
    /// this is the first frame.
    pub fn record(&mut self, buf: &Buffer, metadata: FrameMetadata) {
        if !self.recording {
            return;
        }

        // Evict oldest if at capacity
        if self.snapshots.len() >= self.capacity {
            // When dropping the oldest frame (index 0), we must ensure the
            // next frame (index 1) becomes self-contained (Full snapshot).
            // Otherwise, it remains a Delta relative to the frame we just dropped,
            // making it impossible to reconstruct.
            if self.snapshots.len() > 1 {
                let f0 = &self.snapshots[0];
                let f1 = &self.snapshots[1];

                // Reconstruct the state at frame 1
                let mut buf = Buffer::new(f0.width, f0.height);
                f0.apply_to(&mut buf);
                f1.apply_to(&mut buf);

                // Create a full snapshot from that state
                let f1_full = CompressedFrame::full(&buf).with_cursor(f1.cursor);
                self.snapshots[1] = f1_full;
            }

            self.snapshots.pop_front();
            self.metadata.pop_front();
        }

        let compressed = if self.snapshots.is_empty() {
            CompressedFrame::full(buf)
        } else {
            match &self.last_buffer {
                Some(prev) if prev.width() == buf.width() && prev.height() == buf.height() => {
                    CompressedFrame::delta(buf, prev)
                }
                _ => CompressedFrame::full(buf),
            }
        };

        self.snapshots.push_back(compressed);
        self.metadata.push_back(metadata);
        self.last_buffer = Some(buf.clone());
        self.frame_counter += 1;
    }

    /// Number of recorded snapshots.
    #[inline]
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Check if no frames have been recorded.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Maximum number of snapshots.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Total frame counter (including evicted frames).
    #[inline]
    pub fn frame_counter(&self) -> u64 {
        self.frame_counter
    }

    /// Whether recording is active.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// Pause or resume recording.
    pub fn set_recording(&mut self, recording: bool) {
        self.recording = recording;
    }

    /// Reconstruct the buffer at a given index (0 = oldest retained frame).
    ///
    /// Returns `None` if the index is out of range.
    pub fn get(&self, index: usize) -> Option<Buffer> {
        if index >= self.snapshots.len() {
            return None;
        }

        // The first snapshot is always a full snapshot (or becomes one
        // after eviction resets). We reconstruct by replaying deltas.
        let first = &self.snapshots[0];
        let mut buf = Buffer::new(first.width, first.height);
        for snapshot in self.snapshots.iter().take(index + 1) {
            snapshot.apply_to(&mut buf);
        }
        Some(buf)
    }

    /// Reconstruct the buffer N steps back from the most recent frame.
    ///
    /// `rewind(0)` returns the latest frame. `rewind(1)` returns one step back.
    pub fn rewind(&self, steps: usize) -> Option<Buffer> {
        let index = self.snapshots.len().checked_sub(steps + 1)?;
        self.get(index)
    }

    /// Get metadata for a frame at the given index (0 = oldest).
    pub fn metadata(&self, index: usize) -> Option<&FrameMetadata> {
        self.metadata.get(index)
    }

    /// Get metadata for the most recent frame.
    pub fn latest_metadata(&self) -> Option<&FrameMetadata> {
        self.metadata.back()
    }

    /// Find the index of a frame by model hash.
    ///
    /// Returns the index of the first frame with the matching hash.
    pub fn find_by_hash(&self, hash: u64) -> Option<usize> {
        self.metadata
            .iter()
            .position(|m| m.model_hash == Some(hash))
    }

    /// Total estimated memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        let snapshot_mem: usize = self.snapshots.iter().map(|s| s.memory_size()).sum();
        let metadata_mem = self.metadata.len() * std::mem::size_of::<FrameMetadata>();
        let buf_mem = self
            .last_buffer
            .as_ref()
            .map(|b| b.len() * std::mem::size_of::<Cell>())
            .unwrap_or(0);
        snapshot_mem + metadata_mem + buf_mem + std::mem::size_of::<Self>()
    }

    /// Clear all recorded frames.
    pub fn clear(&mut self) {
        self.snapshots.clear();
        self.metadata.clear();
        self.last_buffer = None;
    }

    // ========================================================================
    // Export / Import
    // ========================================================================

    /// File format magic bytes.
    const MAGIC: &'static [u8] = b"FTUI-TT1";

    /// Export recording to a file.
    ///
    /// Format:
    /// - 8 bytes: magic `FTUI-TT1`
    /// - 2 bytes: width (LE)
    /// - 2 bytes: height (LE)
    /// - 4 bytes: frame count (LE)
    /// - Per frame:
    ///   - 8 bytes: frame_number (LE)
    ///   - 8 bytes: render_time_ns (LE)
    ///   - 4 bytes: event_count (LE)
    ///   - 1 byte: has_model_hash (0/1)
    ///   - 8 bytes: model_hash (if has_model_hash)
    ///   - 4 bytes: change_count (LE)
    ///   - Per change: 20 bytes (CellChange)
    pub fn export(&self, path: &Path) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut w = std::io::BufWriter::new(file);

        // Header
        w.write_all(Self::MAGIC)?;

        let (width, height) = if let Some(first) = self.snapshots.front() {
            (first.width, first.height)
        } else {
            (0, 0)
        };
        w.write_all(&width.to_le_bytes())?;
        w.write_all(&height.to_le_bytes())?;
        w.write_all(&(self.snapshots.len() as u32).to_le_bytes())?;

        // Frames
        for (snapshot, meta) in self.snapshots.iter().zip(self.metadata.iter()) {
            // Metadata
            w.write_all(&meta.frame_number.to_le_bytes())?;
            let render_ns = meta.render_time.as_nanos().min(u64::MAX as u128) as u64;
            w.write_all(&render_ns.to_le_bytes())?;
            w.write_all(&meta.event_count.to_le_bytes())?;

            match meta.model_hash {
                Some(h) => {
                    w.write_all(&[1u8])?;
                    w.write_all(&h.to_le_bytes())?;
                }
                None => {
                    w.write_all(&[0u8])?;
                    w.write_all(&0u64.to_le_bytes())?;
                }
            }

            // Changes
            w.write_all(&(snapshot.changes.len() as u32).to_le_bytes())?;
            for change in &snapshot.changes {
                change.write_to(&mut w)?;
            }
        }

        w.flush()
    }

    /// Import recording from a file.
    pub fn import(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut r = std::io::BufReader::new(file);

        // Header
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != *Self::MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid file format (bad magic)",
            ));
        }

        let mut buf2 = [0u8; 2];
        let mut buf4 = [0u8; 4];
        let mut buf8 = [0u8; 8];

        r.read_exact(&mut buf2)?;
        let width = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf2)?;
        let height = u16::from_le_bytes(buf2);
        r.read_exact(&mut buf4)?;
        let frame_count = u32::from_le_bytes(buf4) as usize;

        let mut tt = Self::new(frame_count.max(1));
        tt.recording = false; // Don't auto-record during import

        for _ in 0..frame_count {
            // Metadata
            r.read_exact(&mut buf8)?;
            let frame_number = u64::from_le_bytes(buf8);
            r.read_exact(&mut buf8)?;
            let render_ns = u64::from_le_bytes(buf8);
            r.read_exact(&mut buf4)?;
            let event_count = u32::from_le_bytes(buf4);

            let mut hash_flag = [0u8; 1];
            r.read_exact(&mut hash_flag)?;
            r.read_exact(&mut buf8)?;
            let model_hash = if hash_flag[0] != 0 {
                Some(u64::from_le_bytes(buf8))
            } else {
                None
            };

            let meta = FrameMetadata {
                frame_number,
                render_time: Duration::from_nanos(render_ns),
                event_count,
                model_hash,
            };

            // Changes
            r.read_exact(&mut buf4)?;
            let change_count = u32::from_le_bytes(buf4) as usize;
            let mut changes = Vec::with_capacity(change_count);
            for _ in 0..change_count {
                changes.push(CellChange::read_from(&mut r)?);
            }

            let snapshot = CompressedFrame {
                width,
                height,
                changes,
                cursor: None,
            };

            tt.snapshots.push_back(snapshot);
            tt.metadata.push_back(meta);
        }

        // Reconstruct last_buffer from final state
        if !tt.snapshots.is_empty() && width > 0 && height > 0 {
            let mut buf = Buffer::new(width, height);
            for snapshot in &tt.snapshots {
                snapshot.apply_to(&mut buf);
            }
            tt.last_buffer = Some(buf);
        }

        tt.frame_counter = frame_count as u64;
        Ok(tt)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::{CellAttrs, PackedRgba, StyleFlags};

    fn make_metadata(n: u64) -> FrameMetadata {
        FrameMetadata::new(n, Duration::from_millis(n + 1))
    }

    #[test]
    fn new_time_travel() {
        let tt = TimeTravel::new(100);
        assert!(tt.is_empty());
        assert_eq!(tt.len(), 0);
        assert_eq!(tt.capacity(), 100);
        assert!(tt.is_recording());
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn zero_capacity_panics() {
        TimeTravel::new(0);
    }

    #[test]
    fn record_single_frame() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(5, 3);
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));

        assert_eq!(tt.len(), 1);
        assert_eq!(tt.frame_counter(), 1);
    }

    #[test]
    fn record_multiple_frames() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(5, 3);

        for i in 0..5u64 {
            buf.set(i as u16, 0, Cell::from_char(char::from(b'A' + i as u8)));
            tt.record(&buf, make_metadata(i));
        }

        assert_eq!(tt.len(), 5);
        assert_eq!(tt.frame_counter(), 5);
    }

    #[test]
    fn capacity_eviction() {
        let mut tt = TimeTravel::new(3);
        let mut buf = Buffer::new(5, 1);

        for i in 0..5u64 {
            buf.set(i as u16 % 5, 0, Cell::from_char(char::from(b'A' + i as u8)));
            tt.record(&buf, make_metadata(i));
        }

        // Only last 3 frames retained
        assert_eq!(tt.len(), 3);
        assert_eq!(tt.frame_counter(), 5);

        // Oldest retained is frame 2
        let meta = tt.metadata(0).unwrap();
        assert_eq!(meta.frame_number, 2);
    }

    #[test]
    fn eviction_preserves_data_integrity() {
        let mut tt = TimeTravel::new(3);
        let mut buf = Buffer::new(5, 1);

        // Frame 0: A....
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));

        // Frame 1: AB... (Delta from 0)
        buf.set(1, 0, Cell::from_char('B'));
        tt.record(&buf, make_metadata(1));

        // Frame 2: ABC.. (Delta from 1)
        buf.set(2, 0, Cell::from_char('C'));
        tt.record(&buf, make_metadata(2));

        // State: [F0, D1, D2]
        // Frame 3: ABCD. (Delta from 2) -> Evicts F0
        buf.set(3, 0, Cell::from_char('D'));
        tt.record(&buf, make_metadata(3));

        // State should be: [F1, D2, D3] (conceptually)
        // Verify we can reconstruct the new head (frame 1)
        let f1 = tt.get(0).unwrap();
        assert_eq!(f1.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(f1.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert!(f1.get(2, 0).unwrap().is_empty());

        // Verify we can reconstruct the tail (frame 3)
        let f3 = tt.get(2).unwrap();
        assert_eq!(f3.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(f3.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(f3.get(2, 0).unwrap().content.as_char(), Some('C'));
        assert_eq!(f3.get(3, 0).unwrap().content.as_char(), Some('D'));
    }

    #[test]
    fn get_reconstructs_frame() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(5, 1);

        // Frame 0: A____
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));

        // Frame 1: AB___
        buf.set(1, 0, Cell::from_char('B'));
        tt.record(&buf, make_metadata(1));

        // Frame 2: ABC__
        buf.set(2, 0, Cell::from_char('C'));
        tt.record(&buf, make_metadata(2));

        // Get frame 0
        let f0 = tt.get(0).unwrap();
        assert_eq!(f0.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert!(f0.get(1, 0).unwrap().is_empty());

        // Get frame 1
        let f1 = tt.get(1).unwrap();
        assert_eq!(f1.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(f1.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert!(f1.get(2, 0).unwrap().is_empty());

        // Get frame 2
        let f2 = tt.get(2).unwrap();
        assert_eq!(f2.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(f2.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(f2.get(2, 0).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn get_out_of_range() {
        let tt = TimeTravel::new(10);
        assert!(tt.get(0).is_none());
        assert!(tt.get(100).is_none());
    }

    #[test]
    fn rewind_from_latest() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(3, 1);

        buf.set(0, 0, Cell::from_char('X'));
        tt.record(&buf, make_metadata(0));

        buf.set(1, 0, Cell::from_char('Y'));
        tt.record(&buf, make_metadata(1));

        buf.set(2, 0, Cell::from_char('Z'));
        tt.record(&buf, make_metadata(2));

        // rewind(0) = latest
        let latest = tt.rewind(0).unwrap();
        assert_eq!(latest.get(2, 0).unwrap().content.as_char(), Some('Z'));

        // rewind(1) = one step back
        let prev = tt.rewind(1).unwrap();
        assert!(prev.get(2, 0).unwrap().is_empty());
        assert_eq!(prev.get(1, 0).unwrap().content.as_char(), Some('Y'));

        // rewind(2) = two steps back
        let oldest = tt.rewind(2).unwrap();
        assert!(oldest.get(1, 0).unwrap().is_empty());
        assert_eq!(oldest.get(0, 0).unwrap().content.as_char(), Some('X'));

        // rewind too far
        assert!(tt.rewind(3).is_none());
    }

    #[test]
    fn pause_resume_recording() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        tt.record(&buf, make_metadata(0));
        assert_eq!(tt.len(), 1);

        tt.set_recording(false);
        assert!(!tt.is_recording());
        tt.record(&buf, make_metadata(1)); // Should be ignored
        assert_eq!(tt.len(), 1);

        tt.set_recording(true);
        tt.record(&buf, make_metadata(2));
        assert_eq!(tt.len(), 2);
    }

    #[test]
    fn metadata_access() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        let meta = FrameMetadata::new(42, Duration::from_millis(5))
            .with_events(3)
            .with_model_hash(0xDEAD);
        tt.record(&buf, meta);

        let stored = tt.metadata(0).unwrap();
        assert_eq!(stored.frame_number, 42);
        assert_eq!(stored.render_time, Duration::from_millis(5));
        assert_eq!(stored.event_count, 3);
        assert_eq!(stored.model_hash, Some(0xDEAD));
    }

    #[test]
    fn latest_metadata() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        assert!(tt.latest_metadata().is_none());

        tt.record(&buf, make_metadata(0));
        tt.record(&buf, make_metadata(1));

        assert_eq!(tt.latest_metadata().unwrap().frame_number, 1);
    }

    #[test]
    fn find_by_hash() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        tt.record(
            &buf,
            FrameMetadata::new(0, Duration::ZERO).with_model_hash(100),
        );
        tt.record(
            &buf,
            FrameMetadata::new(1, Duration::ZERO).with_model_hash(200),
        );
        tt.record(
            &buf,
            FrameMetadata::new(2, Duration::ZERO).with_model_hash(300),
        );

        assert_eq!(tt.find_by_hash(200), Some(1));
        assert_eq!(tt.find_by_hash(999), None);
    }

    #[test]
    fn memory_usage_stays_bounded() {
        let mut tt = TimeTravel::new(5);
        let mut buf = Buffer::new(80, 24);

        // Record many frames, but capacity is 5
        for i in 0..100u64 {
            buf.set((i % 80) as u16, (i % 24) as u16, Cell::from_char('#'));
            tt.record(&buf, make_metadata(i));
        }

        assert_eq!(tt.len(), 5);
        // Memory should be bounded (no unbounded growth)
        let usage = tt.memory_usage();
        assert!(usage < 1_000_000, "memory usage {usage} exceeds 1MB");
    }

    #[test]
    fn clear_resets() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);
        tt.record(&buf, make_metadata(0));
        tt.record(&buf, make_metadata(1));

        tt.clear();
        assert!(tt.is_empty());
        assert_eq!(tt.len(), 0);
    }

    #[test]
    fn compressed_frame_full() {
        let mut buf = Buffer::new(3, 2);
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(2, 1, Cell::from_char('B'));

        let cf = CompressedFrame::full(&buf);
        assert_eq!(cf.change_count(), 2);

        // Apply to empty buffer should reconstruct
        let mut target = Buffer::new(3, 2);
        cf.apply_to(&mut target);
        assert_eq!(target.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(target.get(2, 1).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn compressed_frame_delta() {
        let mut buf1 = Buffer::new(5, 1);
        buf1.set(0, 0, Cell::from_char('A'));
        buf1.set(1, 0, Cell::from_char('B'));

        let mut buf2 = Buffer::new(5, 1);
        buf2.set(0, 0, Cell::from_char('A'));
        buf2.set(1, 0, Cell::from_char('X')); // Changed
        buf2.set(2, 0, Cell::from_char('C')); // Added

        let cf = CompressedFrame::delta(&buf2, &buf1);
        // Only cells that changed (B→X) and added (C) should be stored
        assert_eq!(cf.change_count(), 2);

        // Apply delta to buf1 should produce buf2
        let mut result = buf1.clone();
        cf.apply_to(&mut result);
        assert_eq!(result.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(result.get(1, 0).unwrap().content.as_char(), Some('X'));
        assert_eq!(result.get(2, 0).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn compressed_frame_preserves_style() {
        let mut buf = Buffer::new(3, 1);
        let styled = Cell::from_char('S')
            .with_fg(PackedRgba::rgb(255, 0, 0))
            .with_bg(PackedRgba::rgb(0, 0, 255))
            .with_attrs(CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 42));
        buf.set_raw(0, 0, styled);

        let cf = CompressedFrame::full(&buf);
        let mut target = Buffer::new(3, 1);
        cf.apply_to(&mut target);

        let cell = target.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('S'));
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
        assert_eq!(cell.bg, PackedRgba::rgb(0, 0, 255));
        assert!(cell.attrs.has_flag(StyleFlags::BOLD));
        assert!(cell.attrs.has_flag(StyleFlags::ITALIC));
        assert_eq!(cell.attrs.link_id(), 42);
    }

    #[test]
    fn export_import_roundtrip() {
        let dir = std::env::temp_dir().join("ftui_tt_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.fttr");

        // Create recording
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(5, 2);

        buf.set(0, 0, Cell::from_char('H'));
        buf.set(1, 0, Cell::from_char('i'));
        tt.record(
            &buf,
            FrameMetadata::new(0, Duration::from_millis(1)).with_events(2),
        );

        buf.set(0, 1, Cell::from_char('!'));
        tt.record(
            &buf,
            FrameMetadata::new(1, Duration::from_millis(3))
                .with_events(1)
                .with_model_hash(0xCAFE),
        );

        // Export
        tt.export(&path).unwrap();

        // Import
        let loaded = TimeTravel::import(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify frame 0
        let f0 = loaded.get(0).unwrap();
        assert_eq!(f0.get(0, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(f0.get(1, 0).unwrap().content.as_char(), Some('i'));
        assert!(f0.get(0, 1).unwrap().is_empty());

        // Verify frame 1
        let f1 = loaded.get(1).unwrap();
        assert_eq!(f1.get(0, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(f1.get(0, 1).unwrap().content.as_char(), Some('!'));

        // Verify metadata
        let m0 = loaded.metadata(0).unwrap();
        assert_eq!(m0.frame_number, 0);
        assert_eq!(m0.render_time, Duration::from_millis(1));
        assert_eq!(m0.event_count, 2);

        let m1 = loaded.metadata(1).unwrap();
        assert_eq!(m1.model_hash, Some(0xCAFE));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_invalid_magic() {
        let dir = std::env::temp_dir().join("ftui_tt_bad_magic");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.fttr");

        std::fs::write(&path, b"NOT-MAGIC").unwrap();
        let result = TimeTravel::import(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cell_change_serialization_roundtrip() {
        let change = CellChange {
            x: 42,
            y: 7,
            cell: Cell::from_char('Q')
                .with_fg(PackedRgba::rgb(10, 20, 30))
                .with_bg(PackedRgba::rgb(40, 50, 60))
                .with_attrs(CellAttrs::new(StyleFlags::UNDERLINE, 999)),
        };

        let mut bytes = Vec::new();
        change.write_to(&mut bytes).unwrap();
        assert_eq!(bytes.len(), CellChange::SERIALIZED_SIZE);

        let mut cursor = std::io::Cursor::new(bytes);
        let restored = CellChange::read_from(&mut cursor).unwrap();

        assert_eq!(restored.x, 42);
        assert_eq!(restored.y, 7);
        assert_eq!(restored.cell.content.as_char(), Some('Q'));
        assert_eq!(restored.cell.fg, PackedRgba::rgb(10, 20, 30));
        assert_eq!(restored.cell.bg, PackedRgba::rgb(40, 50, 60));
        assert!(restored.cell.attrs.has_flag(StyleFlags::UNDERLINE));
        assert_eq!(restored.cell.attrs.link_id(), 999);
    }

    #[test]
    fn delta_encoding_efficiency() {
        let mut buf1 = Buffer::new(80, 24);
        for y in 0..24u16 {
            for x in 0..80u16 {
                buf1.set_raw(x, y, Cell::from_char('.'));
            }
        }

        // Change only 5% of cells
        let mut buf2 = buf1.clone();
        for i in 0..96 {
            // 96 / 1920 ≈ 5%
            let x = (i * 7) % 80;
            let y = (i * 3) % 24;
            buf2.set_raw(x as u16, y as u16, Cell::from_char('#'));
        }

        let full = CompressedFrame::full(&buf2);
        let delta = CompressedFrame::delta(&buf2, &buf1);

        // Delta should be much smaller than full
        assert!(delta.change_count() < full.change_count());
        assert!(delta.memory_size() < full.memory_size());
    }

    // ─── Edge-case tests (bd-3127i) ─────────────────────────────

    #[test]
    fn frame_metadata_defaults() {
        let meta = FrameMetadata::new(5, Duration::from_millis(10));
        assert_eq!(meta.frame_number, 5);
        assert_eq!(meta.render_time, Duration::from_millis(10));
        assert_eq!(meta.event_count, 0);
        assert!(meta.model_hash.is_none());
    }

    #[test]
    fn frame_metadata_builder_chain() {
        let meta = FrameMetadata::new(1, Duration::from_millis(2))
            .with_events(10)
            .with_model_hash(0xBEEF);
        assert_eq!(meta.frame_number, 1);
        assert_eq!(meta.event_count, 10);
        assert_eq!(meta.model_hash, Some(0xBEEF));
    }

    #[test]
    fn frame_metadata_debug() {
        let meta = FrameMetadata::new(0, Duration::ZERO);
        let debug = format!("{meta:?}");
        assert!(debug.contains("FrameMetadata"));
    }

    #[test]
    fn frame_metadata_clone() {
        let meta = FrameMetadata::new(7, Duration::from_millis(3)).with_model_hash(42);
        let cloned = meta.clone();
        assert_eq!(cloned.frame_number, 7);
        assert_eq!(cloned.model_hash, Some(42));
    }

    #[test]
    fn compressed_frame_with_cursor() {
        let buf = Buffer::new(3, 1);
        let cf = CompressedFrame::full(&buf).with_cursor(Some((1, 0)));
        assert_eq!(cf.cursor, Some((1, 0)));
    }

    #[test]
    fn compressed_frame_with_cursor_none() {
        let buf = Buffer::new(3, 1);
        let cf = CompressedFrame::full(&buf).with_cursor(None);
        assert_eq!(cf.cursor, None);
    }

    #[test]
    fn compressed_frame_empty_buffer() {
        let buf = Buffer::new(5, 3);
        let cf = CompressedFrame::full(&buf);
        assert_eq!(cf.change_count(), 0, "empty buffer has no changes");
        assert_eq!(cf.width, 5);
        assert_eq!(cf.height, 3);
    }

    #[test]
    fn compressed_frame_delta_identical_buffers() {
        let mut buf = Buffer::new(5, 3);
        buf.set(0, 0, Cell::from_char('X'));
        buf.set(2, 1, Cell::from_char('Y'));

        let delta = CompressedFrame::delta(&buf, &buf);
        assert_eq!(delta.change_count(), 0, "identical buffers have no delta");
    }

    #[test]
    fn compressed_frame_memory_size() {
        let mut buf = Buffer::new(5, 1);
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(1, 0, Cell::from_char('B'));

        let cf = CompressedFrame::full(&buf);
        assert_eq!(cf.change_count(), 2);
        let size = cf.memory_size();
        // Should be at least: struct size + 2 * sizeof(CellChange)
        assert!(
            size >= std::mem::size_of::<CompressedFrame>() + 2 * std::mem::size_of::<CellChange>()
        );
    }

    #[test]
    fn compressed_frame_debug() {
        let buf = Buffer::new(3, 1);
        let cf = CompressedFrame::full(&buf);
        let debug = format!("{cf:?}");
        assert!(debug.contains("CompressedFrame"));
    }

    #[test]
    fn compressed_frame_clone() {
        let mut buf = Buffer::new(3, 1);
        buf.set(0, 0, Cell::from_char('X'));
        let cf = CompressedFrame::full(&buf);
        let cloned = cf.clone();
        assert_eq!(cloned.change_count(), cf.change_count());
        assert_eq!(cloned.width, cf.width);
    }

    #[test]
    fn cell_change_debug_clone_copy_eq() {
        let change = CellChange {
            x: 5,
            y: 3,
            cell: Cell::from_char('Q'),
        };
        // Debug
        let debug = format!("{change:?}");
        assert!(debug.contains("CellChange"));

        // Clone + Copy
        let cloned = change;
        let copied = change; // Copy
        assert_eq!(change, cloned);
        assert_eq!(change, copied);

        // PartialEq with different
        let other = CellChange {
            x: 5,
            y: 3,
            cell: Cell::from_char('R'),
        };
        assert_ne!(change, other);
    }

    #[test]
    fn time_travel_debug() {
        let tt = TimeTravel::new(10);
        let debug = format!("{tt:?}");
        assert!(debug.contains("TimeTravel"));
    }

    #[test]
    fn capacity_one() {
        let mut tt = TimeTravel::new(1);
        let mut buf = Buffer::new(3, 1);

        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));
        assert_eq!(tt.len(), 1);

        buf.set(1, 0, Cell::from_char('B'));
        tt.record(&buf, make_metadata(1));
        assert_eq!(tt.len(), 1, "capacity 1 should keep only latest");
        assert_eq!(tt.frame_counter(), 2);

        // The retained frame should be the latest
        let latest = tt.rewind(0).unwrap();
        assert_eq!(latest.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(latest.get(1, 0).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn clear_preserves_capacity() {
        let mut tt = TimeTravel::new(50);
        let buf = Buffer::new(3, 1);
        tt.record(&buf, make_metadata(0));

        tt.clear();
        assert_eq!(tt.capacity(), 50);
        assert!(tt.is_empty());
    }

    #[test]
    fn clear_then_record() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(3, 1);

        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));
        tt.clear();

        buf.set(0, 0, Cell::from_char('B'));
        tt.record(&buf, make_metadata(1));
        assert_eq!(tt.len(), 1);

        let frame = tt.rewind(0).unwrap();
        assert_eq!(frame.get(0, 0).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn record_different_buffer_sizes_accepted() {
        let mut tt = TimeTravel::new(10);

        let mut buf1 = Buffer::new(5, 3);
        buf1.set(0, 0, Cell::from_char('A'));
        tt.record(&buf1, make_metadata(0));

        // Different size buffer → should trigger full snapshot (not panic)
        let mut buf2 = Buffer::new(10, 5);
        buf2.set(0, 0, Cell::from_char('B'));
        tt.record(&buf2, make_metadata(1));

        assert_eq!(tt.len(), 2);
        assert_eq!(tt.frame_counter(), 2);

        // First frame (same-size) is still accessible
        let frame0 = tt.get(0).unwrap();
        assert_eq!(frame0.get(0, 0).unwrap().content.as_char(), Some('A'));
    }

    #[test]
    fn frame_counter_cumulative_through_eviction() {
        let mut tt = TimeTravel::new(2);
        let buf = Buffer::new(3, 1);

        for i in 0..10u64 {
            tt.record(&buf, make_metadata(i));
        }
        assert_eq!(tt.frame_counter(), 10);
        assert_eq!(tt.len(), 2);
    }

    #[test]
    fn frame_counter_does_not_reset_on_clear() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);
        tt.record(&buf, make_metadata(0));
        tt.record(&buf, make_metadata(1));
        assert_eq!(tt.frame_counter(), 2);

        tt.clear();
        // frame_counter should still be 2 (it's cumulative)
        assert_eq!(tt.frame_counter(), 2);
    }

    #[test]
    fn find_by_hash_returns_first_match() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        tt.record(
            &buf,
            FrameMetadata::new(0, Duration::ZERO).with_model_hash(42),
        );
        tt.record(
            &buf,
            FrameMetadata::new(1, Duration::ZERO).with_model_hash(42),
        );

        // Should return the first occurrence
        assert_eq!(tt.find_by_hash(42), Some(0));
    }

    #[test]
    fn find_by_hash_no_hashes() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        tt.record(&buf, FrameMetadata::new(0, Duration::ZERO));
        tt.record(&buf, FrameMetadata::new(1, Duration::ZERO));

        assert_eq!(tt.find_by_hash(0), None);
        assert_eq!(tt.find_by_hash(42), None);
    }

    #[test]
    fn metadata_out_of_range() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);
        tt.record(&buf, make_metadata(0));

        assert!(tt.metadata(0).is_some());
        assert!(tt.metadata(1).is_none());
        assert!(tt.metadata(999).is_none());
    }

    #[test]
    fn latest_metadata_after_eviction() {
        let mut tt = TimeTravel::new(2);
        let buf = Buffer::new(3, 1);

        tt.record(&buf, make_metadata(0));
        tt.record(&buf, make_metadata(1));
        tt.record(&buf, make_metadata(2));

        assert_eq!(tt.latest_metadata().unwrap().frame_number, 2);
    }

    #[test]
    fn rewind_after_eviction() {
        let mut tt = TimeTravel::new(3);
        let mut buf = Buffer::new(5, 1);

        for i in 0..6u64 {
            buf.set(i as u16 % 5, 0, Cell::from_char(char::from(b'A' + i as u8)));
            tt.record(&buf, make_metadata(i));
        }

        // Only frames 3, 4, 5 retained
        assert_eq!(tt.len(), 3);

        // rewind(0) should be latest (frame 5)
        let latest = tt.rewind(0).unwrap();
        // Frame 5 should have A, B, C, D, F (E at index 0 was overwritten by F)
        assert_eq!(latest.get(0, 0).unwrap().content.as_char(), Some('F'));

        // rewind too far
        assert!(tt.rewind(3).is_none());
    }

    #[test]
    fn multiple_eviction_cycles() {
        let mut tt = TimeTravel::new(3);
        let mut buf = Buffer::new(3, 1);

        // Record 20 frames, each with a unique character at position 0
        for i in 0..20u64 {
            let ch = char::from(b'A' + (i % 26) as u8);
            buf.set(0, 0, Cell::from_char(ch));
            tt.record(&buf, make_metadata(i));
        }

        assert_eq!(tt.len(), 3);
        assert_eq!(tt.frame_counter(), 20);

        // Latest frame should have 'T' (index 19 % 26 = 19 → 'T')
        let latest = tt.rewind(0).unwrap();
        assert_eq!(latest.get(0, 0).unwrap().content.as_char(), Some('T'));
    }

    #[test]
    fn record_when_paused_does_not_increment_counter() {
        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);

        tt.set_recording(false);
        tt.record(&buf, make_metadata(0));
        assert_eq!(tt.frame_counter(), 0);
        assert!(tt.is_empty());
    }

    #[test]
    fn is_empty_after_record() {
        let mut tt = TimeTravel::new(10);
        assert!(tt.is_empty());

        let buf = Buffer::new(3, 1);
        tt.record(&buf, make_metadata(0));
        assert!(!tt.is_empty());
    }

    #[test]
    fn export_empty_recording() {
        let dir = std::env::temp_dir().join("ftui_tt_empty_export");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.fttr");

        let tt = TimeTravel::new(10);
        tt.export(&path).unwrap();

        let loaded = TimeTravel::import(&path).unwrap();
        assert!(loaded.is_empty());
        assert_eq!(loaded.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_import_preserves_styles() {
        let dir = std::env::temp_dir().join("ftui_tt_style_rt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("styled.fttr");

        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(3, 1);
        let styled = Cell::from_char('Z')
            .with_fg(PackedRgba::rgb(100, 200, 50))
            .with_bg(PackedRgba::rgb(10, 20, 30))
            .with_attrs(CellAttrs::new(StyleFlags::BOLD | StyleFlags::UNDERLINE, 7));
        buf.set_raw(0, 0, styled);
        tt.record(&buf, make_metadata(0));

        tt.export(&path).unwrap();
        let loaded = TimeTravel::import(&path).unwrap();

        let frame = loaded.get(0).unwrap();
        let cell = frame.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('Z'));
        assert_eq!(cell.fg, PackedRgba::rgb(100, 200, 50));
        assert_eq!(cell.bg, PackedRgba::rgb(10, 20, 30));
        assert!(cell.attrs.has_flag(StyleFlags::BOLD));
        assert!(cell.attrs.has_flag(StyleFlags::UNDERLINE));
        assert_eq!(cell.attrs.link_id(), 7);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_import_no_model_hash() {
        let dir = std::env::temp_dir().join("ftui_tt_no_hash");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("no_hash.fttr");

        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);
        tt.record(&buf, FrameMetadata::new(0, Duration::from_millis(5)));

        tt.export(&path).unwrap();
        let loaded = TimeTravel::import(&path).unwrap();

        assert!(loaded.metadata(0).unwrap().model_hash.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_usage_positive() {
        let tt = TimeTravel::new(10);
        assert!(
            tt.memory_usage() > 0,
            "empty recorder should still have base size"
        );
    }

    #[test]
    fn memory_usage_grows_with_frames() {
        let mut tt = TimeTravel::new(100);
        let base = tt.memory_usage();

        let mut buf = Buffer::new(10, 5);
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));

        let after_one = tt.memory_usage();
        assert!(after_one > base, "memory should grow after recording");
    }

    #[test]
    fn get_all_frames_after_eviction() {
        let mut tt = TimeTravel::new(3);
        let mut buf = Buffer::new(3, 1);

        // Record 5 frames: A, B, C, D, E
        for i in 0..5u64 {
            buf.set(0, 0, Cell::from_char(char::from(b'A' + i as u8)));
            tt.record(&buf, make_metadata(i));
        }

        // Only 3 retained: frames 2(C), 3(D), 4(E)
        assert_eq!(tt.len(), 3);

        // All 3 frames should be retrievable
        for i in 0..3 {
            assert!(tt.get(i).is_some(), "frame {i} should be retrievable");
        }
        assert!(tt.get(3).is_none());
    }

    #[test]
    fn cell_change_serialized_size() {
        assert_eq!(CellChange::SERIALIZED_SIZE, 20);
    }

    #[test]
    fn compressed_frame_all_cells_changed() {
        let mut buf1 = Buffer::new(3, 2);
        for y in 0..2u16 {
            for x in 0..3u16 {
                buf1.set(x, y, Cell::from_char('.'));
            }
        }

        let mut buf2 = Buffer::new(3, 2);
        for y in 0..2u16 {
            for x in 0..3u16 {
                buf2.set(x, y, Cell::from_char('#'));
            }
        }

        let delta = CompressedFrame::delta(&buf2, &buf1);
        // All 6 cells changed
        assert_eq!(delta.change_count(), 6);
    }

    // ─── Additional edge-case tests (bd-3127i phase 2) ──────────

    #[test]
    fn cell_change_roundtrip_empty_content() {
        use ftui_render::cell::CellContent;

        let change = CellChange {
            x: 0,
            y: 0,
            cell: Cell {
                content: CellContent::EMPTY,
                fg: PackedRgba::TRANSPARENT,
                bg: PackedRgba::TRANSPARENT,
                attrs: CellAttrs::NONE,
            },
        };

        let mut bytes = Vec::new();
        change.write_to(&mut bytes).unwrap();
        assert_eq!(bytes.len(), CellChange::SERIALIZED_SIZE);

        let mut cursor = std::io::Cursor::new(bytes);
        let restored = CellChange::read_from(&mut cursor).unwrap();
        assert!(restored.cell.content.is_empty());
    }

    #[test]
    fn cell_change_roundtrip_continuation_content() {
        use ftui_render::cell::CellContent;

        let change = CellChange {
            x: 10,
            y: 5,
            cell: Cell {
                content: CellContent::CONTINUATION,
                fg: PackedRgba::WHITE,
                bg: PackedRgba::TRANSPARENT,
                attrs: CellAttrs::NONE,
            },
        };

        let mut bytes = Vec::new();
        change.write_to(&mut bytes).unwrap();

        let mut cursor = std::io::Cursor::new(bytes);
        let restored = CellChange::read_from(&mut cursor).unwrap();
        assert!(restored.cell.content.is_continuation());
        assert_eq!(restored.x, 10);
        assert_eq!(restored.y, 5);
    }

    #[test]
    fn cell_change_eq_reflexive_and_symmetric() {
        let a = CellChange {
            x: 1,
            y: 2,
            cell: Cell::from_char('A'),
        };
        let b = CellChange {
            x: 1,
            y: 2,
            cell: Cell::from_char('A'),
        };
        assert_eq!(a, a); // reflexive
        assert_eq!(a, b); // symmetric
        assert_eq!(b, a);
    }

    #[test]
    fn cell_change_ne_different_position() {
        let a = CellChange {
            x: 0,
            y: 0,
            cell: Cell::from_char('A'),
        };
        let b = CellChange {
            x: 1,
            y: 0,
            cell: Cell::from_char('A'),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn capacity_one_with_styled_cells() {
        let mut tt = TimeTravel::new(1);
        let mut buf = Buffer::new(3, 1);

        let styled = Cell::from_char('S')
            .with_fg(PackedRgba::rgb(255, 0, 0))
            .with_bg(PackedRgba::rgb(0, 255, 0))
            .with_attrs(CellAttrs::new(StyleFlags::BOLD, 100));
        buf.set_raw(0, 0, styled);
        tt.record(&buf, make_metadata(0));

        buf.set_raw(1, 0, Cell::from_char('T'));
        tt.record(&buf, make_metadata(1));

        assert_eq!(tt.len(), 1);

        let latest = tt.rewind(0).unwrap();
        let cell = latest.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('S'));
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
        assert!(cell.attrs.has_flag(StyleFlags::BOLD));
        assert_eq!(cell.attrs.link_id(), 100);
    }

    #[test]
    fn multiple_clear_record_cycles() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(3, 1);

        for cycle in 0..5u64 {
            buf.set(0, 0, Cell::from_char(char::from(b'A' + cycle as u8)));
            tt.record(&buf, make_metadata(cycle));
            tt.clear();
            assert!(tt.is_empty());
        }

        // After final clear, record one more
        buf.set(0, 0, Cell::from_char('Z'));
        tt.record(&buf, make_metadata(99));
        assert_eq!(tt.len(), 1);

        let frame = tt.rewind(0).unwrap();
        assert_eq!(frame.get(0, 0).unwrap().content.as_char(), Some('Z'));
    }

    #[test]
    fn memory_usage_decreases_after_clear() {
        let mut tt = TimeTravel::new(100);
        let mut buf = Buffer::new(20, 10);

        for i in 0..50u64 {
            buf.set((i % 20) as u16, (i % 10) as u16, Cell::from_char('#'));
            tt.record(&buf, make_metadata(i));
        }

        let usage_before_clear = tt.memory_usage();
        tt.clear();
        let usage_after_clear = tt.memory_usage();

        assert!(
            usage_after_clear < usage_before_clear,
            "memory should decrease after clear: {usage_after_clear} >= {usage_before_clear}"
        );
    }

    #[test]
    fn minimal_1x1_buffer_record_and_rewind() {
        let mut tt = TimeTravel::new(5);
        let mut buf = Buffer::new(1, 1);

        buf.set(0, 0, Cell::from_char('X'));
        tt.record(&buf, make_metadata(0));

        buf.set(0, 0, Cell::from_char('Y'));
        tt.record(&buf, make_metadata(1));

        let f0 = tt.rewind(1).unwrap();
        assert_eq!(f0.get(0, 0).unwrap().content.as_char(), Some('X'));

        let f1 = tt.rewind(0).unwrap();
        assert_eq!(f1.get(0, 0).unwrap().content.as_char(), Some('Y'));
    }

    #[test]
    fn export_import_after_evictions() {
        let dir = std::env::temp_dir().join("ftui_tt_eviction_export");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("evicted.fttr");

        let mut tt = TimeTravel::new(3);
        let mut buf = Buffer::new(5, 1);

        // Record 6 frames so that 3 evictions occur (capacity=3)
        for i in 0..6u64 {
            buf.set(0, 0, Cell::from_char(char::from(b'A' + i as u8)));
            tt.record(&buf, make_metadata(i));
        }
        assert_eq!(tt.len(), 3);

        // Export after eviction
        tt.export(&path).unwrap();

        // Import and verify data integrity
        let loaded = TimeTravel::import(&path).unwrap();
        assert_eq!(loaded.len(), 3);

        // Verify latest frame (frame 5 → 'F')
        let latest = loaded.get(2).unwrap();
        assert_eq!(latest.get(0, 0).unwrap().content.as_char(), Some('F'));

        // Verify oldest retained (frame 3 → 'D')
        let oldest = loaded.get(0).unwrap();
        assert_eq!(oldest.get(0, 0).unwrap().content.as_char(), Some('D'));

        // Verify metadata of oldest retained
        assert_eq!(loaded.metadata(0).unwrap().frame_number, 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_import_capacity_one() {
        let dir = std::env::temp_dir().join("ftui_tt_cap1_export");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cap1.fttr");

        let mut tt = TimeTravel::new(1);
        let mut buf = Buffer::new(3, 1);

        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));

        buf.set(1, 0, Cell::from_char('B'));
        tt.record(&buf, make_metadata(1)); // Evicts frame 0

        tt.export(&path).unwrap();
        let loaded = TimeTravel::import(&path).unwrap();
        assert_eq!(loaded.len(), 1);

        let frame = loaded.get(0).unwrap();
        assert_eq!(frame.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.get(1, 0).unwrap().content.as_char(), Some('B'));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_change_triggers_full_snapshot() {
        let mut tt = TimeTravel::new(10);

        // Frame 0: 5x1 with 'A'
        let mut buf1 = Buffer::new(5, 1);
        buf1.set(0, 0, Cell::from_char('A'));
        tt.record(&buf1, make_metadata(0));

        // Frame 1: 3x2 with 'B' (different dimensions)
        let mut buf2 = Buffer::new(3, 2);
        buf2.set(0, 0, Cell::from_char('B'));
        tt.record(&buf2, make_metadata(1));

        // Frame 0 should still reconstruct with its own dims
        let f0 = tt.get(0).unwrap();
        assert_eq!(f0.width(), 5);
        assert_eq!(f0.height(), 1);
        assert_eq!(f0.get(0, 0).unwrap().content.as_char(), Some('A'));

        // The snapshot count should reflect both frames
        assert_eq!(tt.len(), 2);
    }

    #[test]
    fn compressed_frame_full_all_non_default() {
        let mut buf = Buffer::new(4, 3);
        for y in 0..3u16 {
            for x in 0..4u16 {
                buf.set(x, y, Cell::from_char('#'));
            }
        }

        let cf = CompressedFrame::full(&buf);
        assert_eq!(cf.change_count(), 12, "all 12 cells are non-default");

        let mut target = Buffer::new(4, 3);
        cf.apply_to(&mut target);
        for y in 0..3u16 {
            for x in 0..4u16 {
                assert_eq!(target.get(x, y).unwrap().content.as_char(), Some('#'));
            }
        }
    }

    #[test]
    fn compressed_frame_cursor_preserved_through_clone() {
        let buf = Buffer::new(3, 1);
        let cf = CompressedFrame::full(&buf).with_cursor(Some((2, 0)));
        let cloned = cf.clone();
        assert_eq!(cloned.cursor, Some((2, 0)));
    }

    #[test]
    fn eviction_rebase_preserves_all_retained_frames() {
        // Exercise the eviction rebase logic (lines 329-345) thoroughly:
        // Record capacity+2 frames, verify all retained frames reconstruct correctly.
        let mut tt = TimeTravel::new(4);
        let mut buf = Buffer::new(5, 1);

        // Frame 0: A____
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));
        // Frame 1: AB___
        buf.set(1, 0, Cell::from_char('B'));
        tt.record(&buf, make_metadata(1));
        // Frame 2: ABC__
        buf.set(2, 0, Cell::from_char('C'));
        tt.record(&buf, make_metadata(2));
        // Frame 3: ABCD_
        buf.set(3, 0, Cell::from_char('D'));
        tt.record(&buf, make_metadata(3));
        // Frame 4: ABCDE → evicts frame 0, rebases frame 1
        buf.set(4, 0, Cell::from_char('E'));
        tt.record(&buf, make_metadata(4));
        // Frame 5: XBCDE → evicts frame 1 (rebased), rebases frame 2
        buf.set(0, 0, Cell::from_char('X'));
        tt.record(&buf, make_metadata(5));

        assert_eq!(tt.len(), 4);

        // Retained: frames 2, 3, 4, 5
        let f2 = tt.get(0).unwrap();
        assert_eq!(f2.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(f2.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(f2.get(2, 0).unwrap().content.as_char(), Some('C'));
        assert!(f2.get(3, 0).unwrap().is_empty());

        let f5 = tt.get(3).unwrap();
        assert_eq!(f5.get(0, 0).unwrap().content.as_char(), Some('X'));
        assert_eq!(f5.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(f5.get(4, 0).unwrap().content.as_char(), Some('E'));
    }

    #[test]
    fn pause_does_not_affect_existing_frames() {
        let mut tt = TimeTravel::new(10);
        let mut buf = Buffer::new(3, 1);

        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, make_metadata(0));

        tt.set_recording(false);

        // Existing frame should still be accessible
        let frame = tt.rewind(0).unwrap();
        assert_eq!(frame.get(0, 0).unwrap().content.as_char(), Some('A'));
    }

    #[test]
    fn find_by_hash_after_eviction() {
        let mut tt = TimeTravel::new(2);
        let buf = Buffer::new(3, 1);

        // Hash 100 will be evicted
        tt.record(
            &buf,
            FrameMetadata::new(0, Duration::ZERO).with_model_hash(100),
        );
        tt.record(
            &buf,
            FrameMetadata::new(1, Duration::ZERO).with_model_hash(200),
        );
        tt.record(
            &buf,
            FrameMetadata::new(2, Duration::ZERO).with_model_hash(300),
        );

        // Hash 100 was evicted
        assert_eq!(tt.find_by_hash(100), None);
        // Hash 200 is now at index 0
        assert_eq!(tt.find_by_hash(200), Some(0));
        // Hash 300 is at index 1
        assert_eq!(tt.find_by_hash(300), Some(1));
    }

    #[test]
    fn frame_metadata_zero_duration() {
        let meta = FrameMetadata::new(0, Duration::ZERO);
        assert_eq!(meta.render_time, Duration::ZERO);
        assert_eq!(meta.frame_number, 0);
    }

    #[test]
    fn frame_metadata_large_values() {
        let meta = FrameMetadata::new(u64::MAX, Duration::from_secs(3600))
            .with_events(u32::MAX)
            .with_model_hash(u64::MAX);
        assert_eq!(meta.frame_number, u64::MAX);
        assert_eq!(meta.event_count, u32::MAX);
        assert_eq!(meta.model_hash, Some(u64::MAX));
    }

    #[test]
    fn export_import_large_values_roundtrip() {
        let dir = std::env::temp_dir().join("ftui_tt_large_vals");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.fttr");

        let mut tt = TimeTravel::new(10);
        let buf = Buffer::new(3, 1);
        let meta = FrameMetadata::new(u64::MAX, Duration::from_secs(3600))
            .with_events(u32::MAX)
            .with_model_hash(u64::MAX);
        tt.record(&buf, meta);

        tt.export(&path).unwrap();
        let loaded = TimeTravel::import(&path).unwrap();

        let m = loaded.metadata(0).unwrap();
        assert_eq!(m.frame_number, u64::MAX);
        assert_eq!(m.event_count, u32::MAX);
        assert_eq!(m.model_hash, Some(u64::MAX));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cell_change_max_coordinates() {
        let change = CellChange {
            x: u16::MAX,
            y: u16::MAX,
            cell: Cell::from_char('Z'),
        };

        let mut bytes = Vec::new();
        change.write_to(&mut bytes).unwrap();

        let mut cursor = std::io::Cursor::new(bytes);
        let restored = CellChange::read_from(&mut cursor).unwrap();
        assert_eq!(restored.x, u16::MAX);
        assert_eq!(restored.y, u16::MAX);
        assert_eq!(restored.cell.content.as_char(), Some('Z'));
    }

    #[test]
    fn compressed_frame_delta_single_cell_change() {
        let mut buf1 = Buffer::new(10, 10);
        buf1.set(5, 5, Cell::from_char('O'));

        let mut buf2 = buf1.clone();
        buf2.set(5, 5, Cell::from_char('X'));

        let delta = CompressedFrame::delta(&buf2, &buf1);
        assert_eq!(delta.change_count(), 1);

        let mut result = buf1.clone();
        delta.apply_to(&mut result);
        assert_eq!(result.get(5, 5).unwrap().content.as_char(), Some('X'));
    }
}
