#![forbid(unsafe_code)]

//! Glyph rasterization + atlas cache (monospace-first).
//!
//! This module intentionally keeps the initial scope narrow:
//! - Deterministic glyph keys/ids suitable for traces and replay.
//! - A single R8 atlas backing store (CPU-side for now).
//! - Explicit eviction policy (LRU) under a fixed byte budget.
//!
//! The WebGPU upload path will be layered on top (queueing dirty rects, etc.).

use std::collections::HashMap;
use std::fmt;

/// Deterministic glyph key.
///
/// For monospace-first terminals, a key is a unicode scalar value + pixel size.
/// Later work (shaping, font fallback, style) can extend this in a backwards-
/// incompatible way (early project; no compat shims).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub codepoint: u32,
    pub px_size: u16,
}

impl GlyphKey {
    #[must_use]
    pub fn from_char(ch: char, px_size: u16) -> Self {
        Self {
            codepoint: ch as u32,
            px_size,
        }
    }
}

/// Deterministic glyph identifier derived from [`GlyphKey`].
///
/// This is stable across runs/platforms (given the same glyph key), and avoids
/// dependence on insertion order.
pub type GlyphId = u64;

/// Monospace glyph metrics needed by the renderer.
///
/// Units are pixels in the font's coordinate system at the given `px_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphMetrics {
    pub advance_x: i16,
    pub bearing_x: i16,
    pub bearing_y: i16,
}

/// Rect within the atlas (in pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl AtlasRect {
    #[must_use]
    pub const fn area_bytes(self) -> usize {
        (self.w as usize) * (self.h as usize)
    }
}

/// Glyph raster output (R8 alpha bitmap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlyphRaster {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u8>,
    pub metrics: GlyphMetrics,
}

impl GlyphRaster {
    #[must_use]
    pub fn bytes_len(&self) -> usize {
        self.pixels.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphPlacement {
    pub id: GlyphId,
    /// Slot rect in the atlas including padding.
    pub slot: AtlasRect,
    /// Draw rect in the atlas (slot minus padding), matching glyph width/height.
    pub draw: AtlasRect,
    pub metrics: GlyphMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub bytes_cached: u64,
    pub bytes_uploaded: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphCacheError {
    /// Glyph (with padding) does not fit in the configured atlas dimensions.
    GlyphTooLarge,
    /// Allocation failed even after eviction; atlas may be fragmented.
    AtlasFull,
    /// Rasterizer returned an invalid bitmap (size mismatch).
    InvalidRaster,
}

impl fmt::Display for GlyphCacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GlyphTooLarge => write!(f, "glyph too large for atlas"),
            Self::AtlasFull => write!(f, "atlas allocation failed (full/fragmented)"),
            Self::InvalidRaster => write!(f, "invalid raster (bitmap size mismatch)"),
        }
    }
}

impl std::error::Error for GlyphCacheError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LruLinks {
    prev: Option<usize>,
    next: Option<usize>,
}

#[derive(Debug, Clone)]
struct Entry {
    key: GlyphKey,
    placement: GlyphPlacement,
    lru: LruLinks,
}

/// Simple R8 atlas backing store with a shelf allocator + free-rect reuse.
#[derive(Debug, Clone)]
struct Atlas {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    cursor_x: u16,
    cursor_y: u16,
    row_h: u16,
    free_slots: Vec<AtlasRect>,
    dirty: Vec<AtlasRect>,
}

impl Atlas {
    fn new(width: u16, height: u16) -> Self {
        let len = (width as usize) * (height as usize);
        Self {
            width,
            height,
            pixels: vec![0u8; len],
            cursor_x: 0,
            cursor_y: 0,
            row_h: 0,
            free_slots: Vec::new(),
            dirty: Vec::new(),
        }
    }

    fn dims(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn take_dirty(&mut self) -> Vec<AtlasRect> {
        std::mem::take(&mut self.dirty)
    }

    fn free_slot(&mut self, slot: AtlasRect) {
        self.free_slots.push(slot);
    }

    fn alloc_slot(&mut self, w: u16, h: u16) -> Option<AtlasRect> {
        // Best-fit reuse from the free list first (avoids fragmentation churn).
        if let Some((idx, _best)) = self
            .free_slots
            .iter()
            .enumerate()
            .filter(|(_, r)| r.w >= w && r.h >= h)
            .min_by_key(|(_, r)| (r.w as u32) * (r.h as u32))
        {
            return Some(self.free_slots.swap_remove(idx));
        }

        // Shelf allocation.
        if self.cursor_x.saturating_add(w) > self.width {
            self.cursor_x = 0;
            self.cursor_y = self.cursor_y.saturating_add(self.row_h);
            self.row_h = 0;
        }
        if self.cursor_y.saturating_add(h) > self.height {
            return None;
        }

        let slot = AtlasRect {
            x: self.cursor_x,
            y: self.cursor_y,
            w,
            h,
        };
        self.cursor_x = self.cursor_x.saturating_add(w);
        self.row_h = self.row_h.max(h);
        Some(slot)
    }

    fn write_r8(
        &mut self,
        dst: AtlasRect,
        src_w: u16,
        src_h: u16,
        src: &[u8],
    ) -> Result<(), GlyphCacheError> {
        if (src_w as usize) * (src_h as usize) != src.len() {
            return Err(GlyphCacheError::InvalidRaster);
        }
        if dst.x.saturating_add(src_w) > self.width || dst.y.saturating_add(src_h) > self.height {
            return Err(GlyphCacheError::InvalidRaster);
        }

        let atlas_w = self.width as usize;
        for row in 0..(src_h as usize) {
            let dst_row = (dst.y as usize + row) * atlas_w + (dst.x as usize);
            let src_row = row * (src_w as usize);
            self.pixels[dst_row..dst_row + (src_w as usize)]
                .copy_from_slice(&src[src_row..src_row + (src_w as usize)]);
        }
        self.dirty.push(AtlasRect {
            x: dst.x,
            y: dst.y,
            w: src_w,
            h: src_h,
        });
        Ok(())
    }
}

/// Glyph atlas cache with LRU eviction under a fixed byte budget.
#[derive(Debug)]
pub struct GlyphAtlasCache {
    atlas: Atlas,
    padding: u16,
    max_cached_bytes: usize,
    cached_bytes: usize,

    // Key -> entry index
    map: HashMap<GlyphKey, usize>,
    // Storage for entries (index-stable).
    entries: Vec<Option<Entry>>,
    // Reuse indices of evicted entries.
    free_entry_indices: Vec<usize>,

    // LRU list head/tail (indices into `entries`).
    lru_head: Option<usize>,
    lru_tail: Option<usize>,

    stats: GlyphCacheStats,
}

impl GlyphAtlasCache {
    /// Create a new cache with a single atlas page.
    ///
    /// `max_cached_bytes` is a hard cap on cached slot area bytes (R8), and must
    /// be <= atlas area bytes.
    pub fn new(atlas_width: u16, atlas_height: u16, max_cached_bytes: usize) -> Self {
        let atlas_area = (atlas_width as usize) * (atlas_height as usize);
        let cap = max_cached_bytes.min(atlas_area);

        Self {
            atlas: Atlas::new(atlas_width, atlas_height),
            padding: 1,
            max_cached_bytes: cap,
            cached_bytes: 0,
            map: HashMap::new(),
            entries: Vec::new(),
            free_entry_indices: Vec::new(),
            lru_head: None,
            lru_tail: None,
            stats: GlyphCacheStats::default(),
        }
    }

    #[must_use]
    pub fn stats(&self) -> GlyphCacheStats {
        self.stats
    }

    #[must_use]
    pub fn atlas_dims(&self) -> (u16, u16) {
        self.atlas.dims()
    }

    /// Take the list of dirty rects written since last call.
    ///
    /// This is intended for future GPU upload scheduling.
    pub fn take_dirty_rects(&mut self) -> Vec<AtlasRect> {
        self.atlas.take_dirty()
    }

    /// Retrieve placement information for a glyph if already cached.
    ///
    /// Hot path: no allocations when present.
    pub fn get(&mut self, key: GlyphKey) -> Option<GlyphPlacement> {
        let idx = *self.map.get(&key)?;
        self.touch(idx);
        self.stats.hits += 1;
        self.entries[idx].as_ref().map(|e| e.placement)
    }

    /// Retrieve placement information for a glyph, inserting on miss.
    ///
    /// The `rasterize` closure is invoked only on cache misses.
    pub fn get_or_insert_with<F>(
        &mut self,
        key: GlyphKey,
        mut rasterize: F,
    ) -> Result<GlyphPlacement, GlyphCacheError>
    where
        F: FnMut(GlyphKey) -> GlyphRaster,
    {
        if let Some(p) = self.get(key) {
            return Ok(p);
        }
        self.stats.misses += 1;
        let raster = rasterize(key);
        self.insert_raster(key, raster)
    }

    fn insert_raster(
        &mut self,
        key: GlyphKey,
        raster: GlyphRaster,
    ) -> Result<GlyphPlacement, GlyphCacheError> {
        let GlyphRaster {
            width,
            height,
            pixels,
            metrics,
        } = raster;

        let expected = (width as usize) * (height as usize);
        if expected != pixels.len() {
            return Err(GlyphCacheError::InvalidRaster);
        }

        let pad = self.padding;
        let slot_w = width.saturating_add(pad.saturating_mul(2));
        let slot_h = height.saturating_add(pad.saturating_mul(2));

        let (atlas_w, atlas_h) = self.atlas_dims();
        if slot_w > atlas_w || slot_h > atlas_h {
            return Err(GlyphCacheError::GlyphTooLarge);
        }

        // Ensure budget headroom by evicting LRU entries.
        let slot_bytes = (slot_w as usize) * (slot_h as usize);
        self.evict_until_within_budget(slot_bytes);

        // Try to allocate, evicting as needed to free reusable slots when shelves are full.
        let slot = self.alloc_slot_with_eviction(slot_w, slot_h)?;

        let draw = AtlasRect {
            x: slot.x + pad,
            y: slot.y + pad,
            w: width,
            h: height,
        };
        self.atlas.write_r8(draw, width, height, &pixels)?;

        let id = glyph_id(key);
        let placement = GlyphPlacement {
            id,
            slot,
            draw,
            metrics,
        };

        let entry = Entry {
            key,
            placement,
            lru: LruLinks {
                prev: None,
                next: None,
            },
        };

        let idx = self.alloc_entry_index();
        self.entries[idx] = Some(entry);
        self.map.insert(key, idx);
        self.push_front(idx);

        self.cached_bytes = self.cached_bytes.saturating_add(slot_bytes);
        self.stats.bytes_cached = self.cached_bytes as u64;
        self.stats.bytes_uploaded = self
            .stats
            .bytes_uploaded
            .saturating_add((width as u64) * (height as u64));

        Ok(placement)
    }

    fn alloc_entry_index(&mut self) -> usize {
        if let Some(idx) = self.free_entry_indices.pop() {
            return idx;
        }
        let idx = self.entries.len();
        self.entries.push(None);
        idx
    }

    fn evict_until_within_budget(&mut self, incoming_slot_bytes: usize) {
        if self.max_cached_bytes == 0 {
            // Degenerate configuration: cache is always empty.
            self.evict_all();
            return;
        }

        while self.cached_bytes.saturating_add(incoming_slot_bytes) > self.max_cached_bytes {
            if self.lru_tail.is_none() {
                break;
            }
            self.evict_one_lru();
        }
    }

    fn alloc_slot_with_eviction(&mut self, w: u16, h: u16) -> Result<AtlasRect, GlyphCacheError> {
        // Fast path: available slot (either free list or shelf).
        if let Some(r) = self.atlas.alloc_slot(w, h) {
            return Ok(r);
        }

        // Shelf is full; try freeing old slots and retry.
        while self.lru_tail.is_some() {
            self.evict_one_lru();
            if let Some(r) = self.atlas.alloc_slot(w, h) {
                return Ok(r);
            }
        }
        Err(GlyphCacheError::AtlasFull)
    }

    fn evict_all(&mut self) {
        while self.lru_tail.is_some() {
            self.evict_one_lru();
        }
    }

    fn evict_one_lru(&mut self) {
        let Some(idx) = self.lru_tail else {
            return;
        };

        self.remove_from_list(idx);
        let Some(entry) = self.entries[idx].take() else {
            return;
        };
        self.map.remove(&entry.key);
        self.atlas.free_slot(entry.placement.slot);
        self.free_entry_indices.push(idx);

        self.cached_bytes = self
            .cached_bytes
            .saturating_sub(entry.placement.slot.area_bytes());
        self.stats.evictions += 1;
        self.stats.bytes_cached = self.cached_bytes as u64;
    }

    fn touch(&mut self, idx: usize) {
        // Move to front.
        if Some(idx) == self.lru_head {
            return;
        }
        self.remove_from_list(idx);
        self.push_front(idx);
    }

    fn push_front(&mut self, idx: usize) {
        let old_head = self.lru_head;
        self.lru_head = Some(idx);
        if self.lru_tail.is_none() {
            self.lru_tail = Some(idx);
        }

        let Some(entry) = self.entries[idx].as_mut() else {
            return;
        };
        entry.lru.prev = None;
        entry.lru.next = old_head;

        if let Some(h) = old_head
            && let Some(head_entry) = self.entries[h].as_mut()
        {
            head_entry.lru.prev = Some(idx);
        }
    }

    fn remove_from_list(&mut self, idx: usize) {
        // Read prev/next via a shared borrow first, then drop it before
        // mutating neighbors (avoids double-mutable-borrow of self.entries).
        let Some(entry) = self.entries[idx].as_ref() else {
            return;
        };
        let prev = entry.lru.prev;
        let next = entry.lru.next;

        if let Some(p) = prev {
            if let Some(p_entry) = self.entries[p].as_mut() {
                p_entry.lru.next = next;
            }
        } else {
            self.lru_head = next;
        }

        if let Some(n) = next {
            if let Some(n_entry) = self.entries[n].as_mut() {
                n_entry.lru.prev = prev;
            }
        } else {
            self.lru_tail = prev;
        }

        if let Some(entry) = self.entries[idx].as_mut() {
            entry.lru.prev = None;
            entry.lru.next = None;
        }
    }
}

/// Stable, deterministic 64-bit hash for glyph keys (FNV-1a).
#[must_use]
pub fn glyph_id(key: GlyphKey) -> GlyphId {
    fnv1a64(&key.codepoint.to_le_bytes(), key.px_size)
}

fn fnv1a64(codepoint_le: &[u8; 4], px_size: u16) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;

    let mut h = FNV_OFFSET;
    for b in codepoint_le {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    for b in px_size.to_le_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster_solid(w: u16, h: u16, metrics: GlyphMetrics) -> GlyphRaster {
        let len = (w as usize) * (h as usize);
        GlyphRaster {
            width: w,
            height: h,
            pixels: vec![0xFF; len],
            metrics,
        }
    }

    #[test]
    fn glyph_id_is_stable_and_distinct_for_different_keys() {
        let a = GlyphKey::from_char('a', 16);
        let b = GlyphKey::from_char('b', 16);
        let a2 = GlyphKey::from_char('a', 18);
        assert_eq!(glyph_id(a), glyph_id(a));
        assert_ne!(glyph_id(a), glyph_id(b));
        assert_ne!(glyph_id(a), glyph_id(a2));
    }

    #[test]
    fn get_or_insert_only_rasterizes_on_miss() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let key = GlyphKey::from_char('x', 12);
        let mut calls = 0u32;

        let _p1 = cache
            .get_or_insert_with(key, |_| {
                calls += 1;
                raster_solid(4, 4, GlyphMetrics::default())
            })
            .expect("insert");
        assert_eq!(calls, 1);

        let _p2 = cache
            .get_or_insert_with(key, |_| {
                calls += 1;
                raster_solid(4, 4, GlyphMetrics::default())
            })
            .expect("hit");
        assert_eq!(calls, 1);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn lru_eviction_happens_under_byte_budget() {
        // Atlas has space, but budget is tiny: only one 8x8 slot at a time.
        let mut cache = GlyphAtlasCache::new(64, 64, 8 * 8);
        let k1 = GlyphKey::from_char('a', 16);
        let k2 = GlyphKey::from_char('b', 16);

        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k1");
        assert!(cache.get(k1).is_some());

        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k2");

        // Budget forces eviction of k1 (LRU).
        assert!(cache.get(k1).is_none());
        assert!(cache.get(k2).is_some());
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn freed_slots_can_be_reused() {
        // Force eviction by budget to produce a free slot.
        let mut cache = GlyphAtlasCache::new(32, 32, 12 * 12);
        let k1 = GlyphKey::from_char('a', 16);
        let k2 = GlyphKey::from_char('b', 16);

        let p1 = cache
            .get_or_insert_with(k1, |_| raster_solid(10, 10, GlyphMetrics::default()))
            .expect("k1");
        let _p2 = cache
            .get_or_insert_with(k2, |_| raster_solid(10, 10, GlyphMetrics::default()))
            .expect("k2");

        // k1 should have been evicted; new insert should have a chance to reuse its slot.
        assert!(cache.get(k1).is_none());
        let k3 = GlyphKey::from_char('c', 16);
        let p3 = cache
            .get_or_insert_with(k3, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k3");

        // Best-fit should pick the freed slot (same slot origin).
        assert_eq!(p3.slot.x, p1.slot.x);
        assert_eq!(p3.slot.y, p1.slot.y);
    }
}
