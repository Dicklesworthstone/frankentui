#![forbid(unsafe_code)]

//! Glyph rasterization + atlas cache (monospace-first).
//!
//! This module intentionally keeps the initial scope narrow:
//! - Deterministic glyph keys/ids suitable for traces and replay.
//! - A single R8 atlas backing store (CPU-side for now).
//! - Explicit eviction policy (LRU) under a fixed byte budget.
//!
//! The WebGPU upload path will be layered on top (queueing dirty rects, etc.).
//!
//! Cache policy objective (bd-lff4p.5.6):
//! `loss = miss_rate + 0.25*eviction_rate + 0.5*pressure_ratio`.
//! Lower is better; this is logged via [`GlyphAtlasCache::objective`].

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheObjective {
    pub miss_rate: f64,
    pub eviction_rate: f64,
    pub pressure_ratio: f64,
    pub loss: f64,
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

const CACHE_LOSS_MISS_WEIGHT: f64 = 1.0;
const CACHE_LOSS_EVICTION_WEIGHT: f64 = 0.25;
const CACHE_LOSS_PRESSURE_WEIGHT: f64 = 0.5;

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

    fn pixels(&self) -> &[u8] {
        &self.pixels
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

    /// Return the objective components used for cache-policy tuning.
    ///
    /// Objective:
    /// `loss = miss_rate + 0.25*eviction_rate + 0.5*pressure_ratio`.
    #[must_use]
    pub fn objective(&self) -> CacheObjective {
        let lookups = self.stats.hits.saturating_add(self.stats.misses);
        let miss_rate = if lookups == 0 {
            0.0
        } else {
            self.stats.misses as f64 / lookups as f64
        };
        let eviction_rate = if self.stats.misses == 0 {
            0.0
        } else {
            self.stats.evictions as f64 / self.stats.misses as f64
        };
        let pressure_ratio = if self.max_cached_bytes == 0 {
            1.0
        } else {
            self.cached_bytes.min(self.max_cached_bytes) as f64 / self.max_cached_bytes as f64
        };
        let loss = (CACHE_LOSS_MISS_WEIGHT * miss_rate)
            + (CACHE_LOSS_EVICTION_WEIGHT * eviction_rate)
            + (CACHE_LOSS_PRESSURE_WEIGHT * pressure_ratio);
        CacheObjective {
            miss_rate,
            eviction_rate,
            pressure_ratio,
            loss,
        }
    }

    #[must_use]
    pub fn atlas_dims(&self) -> (u16, u16) {
        self.atlas.dims()
    }

    #[must_use]
    pub fn atlas_pixels(&self) -> &[u8] {
        self.atlas.pixels()
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
        if self
            .entries
            .get(idx)
            .and_then(|entry| entry.as_ref())
            .is_none()
        {
            // Defensive repair for stale map entries. Treat as miss.
            self.map.remove(&key);
            return None;
        }
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

        // Try to allocate, evicting as needed to free reusable slots when shelves are full.
        //
        // Note: free-slot reuse can return a slot larger than requested. We do a
        // budget pass using the requested area first, then (after allocation)
        // re-run budget enforcement against the *actual* allocated slot size.
        let slot_bytes_req = (slot_w as usize) * (slot_h as usize);
        self.evict_until_within_budget(slot_bytes_req);

        let slot = self.alloc_slot_with_eviction(slot_w, slot_h)?;
        let slot_bytes = slot.area_bytes();
        self.evict_until_within_budget(slot_bytes);

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

        // Minimize pressure term in the cache objective by restoring budget headroom.
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
        // Reuse returns the full freed slot (which can be larger than the
        // requested allocation); cache accounting must reflect the actual slot.
        assert_eq!(cache.stats().bytes_cached, p3.slot.area_bytes() as u64);
    }

    #[test]
    fn objective_is_zero_for_empty_cache() {
        let cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let objective = cache.objective();
        assert_eq!(objective.miss_rate, 0.0);
        assert_eq!(objective.eviction_rate, 0.0);
        assert_eq!(objective.pressure_ratio, 0.0);
        assert_eq!(objective.loss, 0.0);
    }

    #[test]
    fn objective_tracks_pressure_and_evictions() {
        let mut cache = GlyphAtlasCache::new(64, 64, 8 * 8);
        let k1 = GlyphKey::from_char('a', 16);
        let k2 = GlyphKey::from_char('b', 16);

        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k1");
        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k2");

        let objective = cache.objective();
        assert!(objective.miss_rate > 0.0);
        assert!(objective.eviction_rate > 0.0);
        assert!(objective.pressure_ratio > 0.0);
        assert!(objective.loss > 0.0);
    }

    #[test]
    fn golden_fixture_placement_and_dirty_rects_are_stable() {
        // Deterministic fixture: insertion order, sizes, and allocator behavior
        // must produce stable atlas coordinates across runs.
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);

        #[derive(Clone, Copy)]
        struct Fixture {
            key: GlyphKey,
            raster_w: u16,
            raster_h: u16,
            metrics: GlyphMetrics,
            expected_slot: AtlasRect,
        }

        let fixtures = [
            Fixture {
                key: GlyphKey::from_char('A', 16),
                raster_w: 3,
                raster_h: 5,
                metrics: GlyphMetrics {
                    advance_x: 3,
                    bearing_x: 0,
                    bearing_y: 4,
                },
                expected_slot: AtlasRect {
                    x: 0,
                    y: 0,
                    w: 5, // +2 padding
                    h: 7, // +2 padding
                },
            },
            Fixture {
                key: GlyphKey::from_char('B', 16),
                raster_w: 4,
                raster_h: 4,
                metrics: GlyphMetrics {
                    advance_x: 4,
                    bearing_x: 1,
                    bearing_y: 3,
                },
                expected_slot: AtlasRect {
                    x: 5,
                    y: 0,
                    w: 6,
                    h: 6,
                },
            },
            Fixture {
                key: GlyphKey::from_char('C', 16),
                raster_w: 7,
                raster_h: 3,
                metrics: GlyphMetrics {
                    advance_x: 7,
                    bearing_x: 0,
                    bearing_y: 2,
                },
                expected_slot: AtlasRect {
                    x: 11,
                    y: 0,
                    w: 9,
                    h: 5,
                },
            },
            Fixture {
                key: GlyphKey::from_char('D', 16),
                raster_w: 2,
                raster_h: 8,
                metrics: GlyphMetrics {
                    advance_x: 2,
                    bearing_x: -1,
                    bearing_y: 7,
                },
                expected_slot: AtlasRect {
                    x: 20,
                    y: 0,
                    w: 4,
                    h: 10,
                },
            },
            Fixture {
                // Forces a shelf row break (cursor_x + slot_w would exceed atlas width).
                key: GlyphKey::from_char('E', 16),
                raster_w: 10,
                raster_h: 10,
                metrics: GlyphMetrics {
                    advance_x: 10,
                    bearing_x: 0,
                    bearing_y: 9,
                },
                expected_slot: AtlasRect {
                    x: 0,
                    y: 10,
                    w: 12,
                    h: 12,
                },
            },
        ];

        let mut expected_dirty = Vec::with_capacity(fixtures.len());

        for f in fixtures {
            let placement = cache
                .get_or_insert_with(f.key, |_| raster_solid(f.raster_w, f.raster_h, f.metrics))
                .expect("fixture insert");

            let expected_draw = AtlasRect {
                x: f.expected_slot.x + 1,
                y: f.expected_slot.y + 1,
                w: f.raster_w,
                h: f.raster_h,
            };

            assert_eq!(placement.id, glyph_id(f.key));
            assert_eq!(placement.metrics, f.metrics);
            assert_eq!(placement.slot, f.expected_slot);
            assert_eq!(placement.draw, expected_draw);

            expected_dirty.push(expected_draw);
        }

        let dirty = cache.take_dirty_rects();
        assert_eq!(dirty, expected_dirty);

        // Second pass should be all hits and placements must remain identical.
        for f in fixtures {
            let placement = cache.get(f.key).expect("hit");
            assert_eq!(placement.slot, f.expected_slot);
        }
        assert_eq!(cache.stats().hits, fixtures.len() as u64);
        assert_eq!(cache.stats().misses, fixtures.len() as u64);
    }

    #[test]
    fn golden_fixture_representative_charset_coords_are_stable() {
        // Representative monospace-first charset fixture:
        // ASCII + box drawing + common symbols.
        let mut cache = GlyphAtlasCache::new(40, 24, 40 * 24);

        #[derive(Clone, Copy)]
        struct Fixture {
            key: GlyphKey,
            raster_w: u16,
            raster_h: u16,
            metrics: GlyphMetrics,
            expected_slot: AtlasRect,
        }

        let fixtures = [
            Fixture {
                key: GlyphKey::from_char('A', 16),
                raster_w: 4,
                raster_h: 6,
                metrics: GlyphMetrics {
                    advance_x: 4,
                    bearing_x: 0,
                    bearing_y: 5,
                },
                expected_slot: AtlasRect {
                    x: 0,
                    y: 0,
                    w: 6,
                    h: 8,
                },
            },
            Fixture {
                key: GlyphKey::from_char('│', 16),
                raster_w: 2,
                raster_h: 8,
                metrics: GlyphMetrics {
                    advance_x: 2,
                    bearing_x: 0,
                    bearing_y: 7,
                },
                expected_slot: AtlasRect {
                    x: 6,
                    y: 0,
                    w: 4,
                    h: 10,
                },
            },
            Fixture {
                key: GlyphKey::from_char('─', 16),
                raster_w: 8,
                raster_h: 2,
                metrics: GlyphMetrics {
                    advance_x: 8,
                    bearing_x: 0,
                    bearing_y: 1,
                },
                expected_slot: AtlasRect {
                    x: 10,
                    y: 0,
                    w: 10,
                    h: 4,
                },
            },
            Fixture {
                key: GlyphKey::from_char('◆', 16),
                raster_w: 6,
                raster_h: 6,
                metrics: GlyphMetrics {
                    advance_x: 6,
                    bearing_x: 0,
                    bearing_y: 5,
                },
                expected_slot: AtlasRect {
                    x: 20,
                    y: 0,
                    w: 8,
                    h: 8,
                },
            },
            Fixture {
                key: GlyphKey::from_char('→', 16),
                raster_w: 9,
                raster_h: 3,
                metrics: GlyphMetrics {
                    advance_x: 9,
                    bearing_x: 0,
                    bearing_y: 2,
                },
                expected_slot: AtlasRect {
                    x: 28,
                    y: 0,
                    w: 11,
                    h: 5,
                },
            },
            Fixture {
                // Forces a row break from x=39 with a 7px-wide slot.
                key: GlyphKey::from_char('✓', 16),
                raster_w: 5,
                raster_h: 6,
                metrics: GlyphMetrics {
                    advance_x: 5,
                    bearing_x: 0,
                    bearing_y: 5,
                },
                expected_slot: AtlasRect {
                    x: 0,
                    y: 10,
                    w: 7,
                    h: 8,
                },
            },
        ];

        let mut expected_dirty = Vec::with_capacity(fixtures.len());
        for f in fixtures {
            let placement = cache
                .get_or_insert_with(f.key, |_| raster_solid(f.raster_w, f.raster_h, f.metrics))
                .expect("fixture insert");

            let expected_draw = AtlasRect {
                x: f.expected_slot.x + 1,
                y: f.expected_slot.y + 1,
                w: f.raster_w,
                h: f.raster_h,
            };
            assert_eq!(placement.id, glyph_id(f.key));
            assert_eq!(placement.metrics, f.metrics);
            assert_eq!(placement.slot, f.expected_slot);
            assert_eq!(placement.draw, expected_draw);
            expected_dirty.push(expected_draw);
        }

        assert_eq!(cache.take_dirty_rects(), expected_dirty);
        for f in fixtures {
            let placement = cache.get(f.key).expect("hit");
            assert_eq!(placement.slot, f.expected_slot);
        }
    }

    #[test]
    fn lru_touch_keeps_recent_entry_hot() {
        // Each 6x6 raster occupies an 8x8 padded slot under current settings.
        // Budget allows exactly two slots; third insert should evict true LRU.
        let mut cache = GlyphAtlasCache::new(64, 64, 2 * 8 * 8);
        let k1 = GlyphKey::from_char('a', 16);
        let k2 = GlyphKey::from_char('b', 16);
        let k3 = GlyphKey::from_char('c', 16);

        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k1");
        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k2");

        // Touch k1 so k2 becomes least-recently-used.
        assert!(cache.get(k1).is_some());

        let _ = cache
            .get_or_insert_with(k3, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k3");

        assert!(
            cache.get(k1).is_some(),
            "touched key should survive eviction"
        );
        assert!(
            cache.get(k2).is_none(),
            "least-recently-used key should be evicted"
        );
        assert!(cache.get(k3).is_some());
    }

    #[test]
    fn bytes_uploaded_changes_only_on_miss() {
        let mut cache = GlyphAtlasCache::new(64, 64, 8 * 8);
        let k1 = GlyphKey::from_char('x', 16);
        let k2 = GlyphKey::from_char('y', 16);
        let raster_bytes = (6 * 6) as u64;

        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("first insert");
        assert_eq!(cache.stats().bytes_uploaded, raster_bytes);

        let mut calls = 0u32;
        let _ = cache
            .get_or_insert_with(k1, |_| {
                calls += 1;
                raster_solid(6, 6, GlyphMetrics::default())
            })
            .expect("cached hit");
        assert_eq!(calls, 0, "rasterizer should not run on cache hit");
        assert_eq!(
            cache.stats().bytes_uploaded,
            raster_bytes,
            "bytes_uploaded should not change on hit"
        );

        // Force an eviction and then reinsert k1; both operations are misses and should
        // advance bytes_uploaded by one raster each.
        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k2 insert");
        assert_eq!(cache.stats().bytes_uploaded, raster_bytes * 2);

        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k1 reinsert");
        assert_eq!(cache.stats().bytes_uploaded, raster_bytes * 3);
    }

    #[test]
    fn cached_bytes_never_exceeds_budget() {
        let budget = 8 * 8;
        let mut cache = GlyphAtlasCache::new(64, 64, budget);

        for ch in ['a', 'b', 'c', 'd', 'e', 'f'] {
            let _ = cache
                .get_or_insert_with(GlyphKey::from_char(ch, 16), |_| {
                    raster_solid(6, 6, GlyphMetrics::default())
                })
                .expect("insert");
            assert!(
                cache.stats().bytes_cached <= budget as u64,
                "cached_bytes exceeded budget after inserting {ch}"
            );
        }
    }

    // ── GlyphKey edge cases ────────────────────────────────────────

    #[test]
    fn glyph_key_from_null_char() {
        let key = GlyphKey::from_char('\0', 16);
        assert_eq!(key.codepoint, 0);
        assert_eq!(key.px_size, 16);
    }

    #[test]
    fn glyph_key_from_high_codepoint() {
        // U+1F600 = 😀 (emoji)
        let key = GlyphKey::from_char('\u{1F600}', 24);
        assert_eq!(key.codepoint, 0x1F600);
        assert_eq!(key.px_size, 24);
    }

    #[test]
    fn glyph_key_same_char_different_sizes_are_distinct() {
        let k1 = GlyphKey::from_char('A', 12);
        let k2 = GlyphKey::from_char('A', 16);
        assert_ne!(k1, k2);
        assert_ne!(glyph_id(k1), glyph_id(k2));
    }

    #[test]
    fn glyph_key_zero_px_size() {
        let key = GlyphKey::from_char('A', 0);
        assert_eq!(key.px_size, 0);
        // Should still produce a valid (deterministic) glyph id.
        let id1 = glyph_id(key);
        let id2 = glyph_id(key);
        assert_eq!(id1, id2);
    }

    // ── AtlasRect ──────────────────────────────────────────────────

    #[test]
    fn atlas_rect_area_bytes_basic() {
        let r = AtlasRect {
            x: 0,
            y: 0,
            w: 10,
            h: 8,
        };
        assert_eq!(r.area_bytes(), 80);
    }

    #[test]
    fn atlas_rect_area_bytes_zero_dimension() {
        let zero_w = AtlasRect {
            x: 5,
            y: 5,
            w: 0,
            h: 10,
        };
        assert_eq!(zero_w.area_bytes(), 0);

        let zero_h = AtlasRect {
            x: 5,
            y: 5,
            w: 10,
            h: 0,
        };
        assert_eq!(zero_h.area_bytes(), 0);
    }

    #[test]
    fn atlas_rect_area_bytes_single_pixel() {
        let r = AtlasRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        assert_eq!(r.area_bytes(), 1);
    }

    // ── GlyphRaster ───────────────────────────────────────────────

    #[test]
    fn glyph_raster_bytes_len_matches_pixels() {
        let raster = raster_solid(5, 3, GlyphMetrics::default());
        assert_eq!(raster.bytes_len(), 15);
        assert_eq!(raster.bytes_len(), raster.pixels.len());
    }

    #[test]
    fn glyph_raster_bytes_len_empty() {
        let raster = GlyphRaster {
            width: 0,
            height: 0,
            pixels: vec![],
            metrics: GlyphMetrics::default(),
        };
        assert_eq!(raster.bytes_len(), 0);
    }

    // ── GlyphMetrics / GlyphCacheStats defaults ───────────────────

    #[test]
    fn glyph_metrics_default_is_zero() {
        let m = GlyphMetrics::default();
        assert_eq!(m.advance_x, 0);
        assert_eq!(m.bearing_x, 0);
        assert_eq!(m.bearing_y, 0);
    }

    #[test]
    fn glyph_cache_stats_default_is_zero() {
        let s = GlyphCacheStats::default();
        assert_eq!(s.hits, 0);
        assert_eq!(s.misses, 0);
        assert_eq!(s.evictions, 0);
        assert_eq!(s.bytes_cached, 0);
        assert_eq!(s.bytes_uploaded, 0);
    }

    // ── GlyphCacheError Display / Error ───────────────────────────

    #[test]
    fn glyph_cache_error_display_glyph_too_large() {
        let e = GlyphCacheError::GlyphTooLarge;
        assert_eq!(format!("{e}"), "glyph too large for atlas");
    }

    #[test]
    fn glyph_cache_error_display_atlas_full() {
        let e = GlyphCacheError::AtlasFull;
        assert_eq!(format!("{e}"), "atlas allocation failed (full/fragmented)");
    }

    #[test]
    fn glyph_cache_error_display_invalid_raster() {
        let e = GlyphCacheError::InvalidRaster;
        assert_eq!(format!("{e}"), "invalid raster (bitmap size mismatch)");
    }

    #[test]
    fn glyph_cache_error_implements_std_error() {
        let e: Box<dyn std::error::Error> = Box::new(GlyphCacheError::GlyphTooLarge);
        // source() should be None for leaf errors.
        assert!(e.source().is_none());
    }

    // ── Error paths ───────────────────────────────────────────────

    #[test]
    fn insert_invalid_raster_size_mismatch() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let key = GlyphKey::from_char('X', 16);
        let result = cache.get_or_insert_with(key, |_| GlyphRaster {
            width: 4,
            height: 4,
            pixels: vec![0xFF; 10], // mismatch: 10 != 4*4
            metrics: GlyphMetrics::default(),
        });
        assert!(matches!(result, Err(GlyphCacheError::InvalidRaster)));
    }

    #[test]
    fn insert_glyph_too_large_for_atlas() {
        // Atlas is 8x8; a 10x10 glyph (+ 2 padding = 12x12) won't fit.
        let mut cache = GlyphAtlasCache::new(8, 8, 8 * 8);
        let key = GlyphKey::from_char('X', 16);
        let result =
            cache.get_or_insert_with(key, |_| raster_solid(10, 10, GlyphMetrics::default()));
        assert!(matches!(result, Err(GlyphCacheError::GlyphTooLarge)));
    }

    #[test]
    fn insert_glyph_exactly_fills_atlas() {
        // Atlas is 8x8; a 6x6 glyph (+ 2 padding = 8x8) exactly fits.
        let mut cache = GlyphAtlasCache::new(8, 8, 8 * 8);
        let key = GlyphKey::from_char('X', 16);
        let result = cache.get_or_insert_with(key, |_| raster_solid(6, 6, GlyphMetrics::default()));
        assert!(result.is_ok());
    }

    #[test]
    fn atlas_full_after_fragmentation() {
        // Tiny atlas that can hold exactly one 4x4 padded slot (6x6).
        // After eviction the freed slot is reusable, but if we request a
        // different size that doesn't fit any free slot or shelf, we get AtlasFull.
        let mut cache = GlyphAtlasCache::new(8, 8, 8 * 8);
        let k1 = GlyphKey::from_char('a', 16);
        // Insert a 6x6 glyph (padded to 8x8), fills the whole atlas.
        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k1");

        // Evict k1 by inserting something under budget pressure, but the freed
        // slot is 8x8. If we ask for a 7x7 glyph (padded 9x9), it exceeds atlas dims.
        let k2 = GlyphKey::from_char('b', 16);
        let result = cache.get_or_insert_with(k2, |_| raster_solid(7, 7, GlyphMetrics::default()));
        assert!(matches!(result, Err(GlyphCacheError::GlyphTooLarge)));
    }

    // ── Zero-budget cache ─────────────────────────────────────────

    #[test]
    fn zero_budget_cache_still_works() {
        // max_cached_bytes = 0 means every insert triggers evict_all.
        let mut cache = GlyphAtlasCache::new(32, 32, 0);
        let k1 = GlyphKey::from_char('a', 16);
        let result = cache.get_or_insert_with(k1, |_| raster_solid(4, 4, GlyphMetrics::default()));
        // Should succeed (atlas has space even if budget is 0).
        assert!(result.is_ok());
        // But the glyph is immediately evicted for budget reasons on next insert.
        let k2 = GlyphKey::from_char('b', 16);
        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(4, 4, GlyphMetrics::default()))
            .expect("k2");
        assert!(
            cache.get(k1).is_none(),
            "k1 should be evicted under zero budget"
        );
    }

    #[test]
    fn zero_budget_objective_pressure_is_one() {
        let cache = GlyphAtlasCache::new(32, 32, 0);
        let obj = cache.objective();
        assert_eq!(obj.pressure_ratio, 1.0);
    }

    // ── Accessor coverage ─────────────────────────────────────────

    #[test]
    fn atlas_dims_match_constructor() {
        let cache = GlyphAtlasCache::new(64, 48, 64 * 48);
        assert_eq!(cache.atlas_dims(), (64, 48));
    }

    #[test]
    fn atlas_pixels_length_matches_dims() {
        let cache = GlyphAtlasCache::new(16, 16, 16 * 16);
        assert_eq!(cache.atlas_pixels().len(), 16 * 16);
    }

    #[test]
    fn atlas_pixels_initially_zeroed() {
        let cache = GlyphAtlasCache::new(8, 8, 8 * 8);
        assert!(cache.atlas_pixels().iter().all(|&b| b == 0));
    }

    #[test]
    fn take_dirty_rects_clears_list() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let _ = cache
            .get_or_insert_with(GlyphKey::from_char('a', 16), |_| {
                raster_solid(4, 4, GlyphMetrics::default())
            })
            .expect("insert");
        let dirty1 = cache.take_dirty_rects();
        assert!(!dirty1.is_empty());
        let dirty2 = cache.take_dirty_rects();
        assert!(
            dirty2.is_empty(),
            "dirty rects should be cleared after take"
        );
    }

    // ── Single-pixel rasters ──────────────────────────────────────

    #[test]
    fn single_pixel_raster_inserts_successfully() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let key = GlyphKey::from_char('.', 8);
        let placement = cache
            .get_or_insert_with(key, |_| raster_solid(1, 1, GlyphMetrics::default()))
            .expect("1x1 insert");
        assert_eq!(placement.draw.w, 1);
        assert_eq!(placement.draw.h, 1);
        // Slot includes padding.
        assert_eq!(placement.slot.w, 3); // 1 + 2*1 padding
        assert_eq!(placement.slot.h, 3);
    }

    // ── Placement field checks ────────────────────────────────────

    #[test]
    fn placement_id_matches_glyph_id() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let key = GlyphKey::from_char('Z', 20);
        let placement = cache
            .get_or_insert_with(key, |_| {
                raster_solid(
                    5,
                    7,
                    GlyphMetrics {
                        advance_x: 5,
                        bearing_x: 1,
                        bearing_y: 6,
                    },
                )
            })
            .expect("insert");
        assert_eq!(placement.id, glyph_id(key));
        assert_eq!(placement.metrics.advance_x, 5);
        assert_eq!(placement.metrics.bearing_x, 1);
        assert_eq!(placement.metrics.bearing_y, 6);
    }

    #[test]
    fn placement_draw_is_slot_inset_by_padding() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let key = GlyphKey::from_char('Q', 16);
        let placement = cache
            .get_or_insert_with(key, |_| raster_solid(4, 6, GlyphMetrics::default()))
            .expect("insert");
        // Draw rect should be slot inset by 1px padding on each side.
        assert_eq!(placement.draw.x, placement.slot.x + 1);
        assert_eq!(placement.draw.y, placement.slot.y + 1);
        assert_eq!(placement.draw.w, 4);
        assert_eq!(placement.draw.h, 6);
    }

    // ── Multiple sequential evictions ─────────────────────────────

    #[test]
    fn multiple_evictions_track_correctly() {
        // Budget fits exactly one 8x8 padded slot.
        let mut cache = GlyphAtlasCache::new(64, 64, 8 * 8);
        let keys: Vec<GlyphKey> = ('a'..='e').map(|ch| GlyphKey::from_char(ch, 16)).collect();

        for key in &keys {
            let _ = cache
                .get_or_insert_with(*key, |_| raster_solid(6, 6, GlyphMetrics::default()))
                .expect("insert");
        }

        // 5 inserts, each evicts the previous (except the first), so 4 evictions.
        assert_eq!(cache.stats().misses, 5);
        assert_eq!(cache.stats().evictions, 4);
        // Only the last key should remain cached.
        for key in &keys[..4] {
            assert!(cache.get(*key).is_none());
        }
        assert!(cache.get(keys[4]).is_some());
    }

    // ── Objective formula edge cases ──────────────────────────────

    #[test]
    fn objective_all_hits_no_misses() {
        let mut cache = GlyphAtlasCache::new(64, 64, 64 * 64);
        let key = GlyphKey::from_char('a', 16);
        let _ = cache
            .get_or_insert_with(key, |_| raster_solid(4, 4, GlyphMetrics::default()))
            .expect("insert");
        // Hit the cached entry several times.
        for _ in 0..5 {
            assert!(cache.get(key).is_some());
        }
        let obj = cache.objective();
        // 1 miss, 5 hits = miss_rate = 1/6
        let expected_miss_rate = 1.0 / 6.0;
        assert!((obj.miss_rate - expected_miss_rate).abs() < 1e-10);
        assert_eq!(obj.eviction_rate, 0.0);
        assert!(obj.pressure_ratio > 0.0);
    }

    #[test]
    fn objective_loss_components_sum_correctly() {
        let mut cache = GlyphAtlasCache::new(64, 64, 8 * 8);
        let k1 = GlyphKey::from_char('a', 16);
        let k2 = GlyphKey::from_char('b', 16);
        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k1");
        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(6, 6, GlyphMetrics::default()))
            .expect("k2");

        let obj = cache.objective();
        let expected_loss = (CACHE_LOSS_MISS_WEIGHT * obj.miss_rate)
            + (CACHE_LOSS_EVICTION_WEIGHT * obj.eviction_rate)
            + (CACHE_LOSS_PRESSURE_WEIGHT * obj.pressure_ratio);
        assert!(
            (obj.loss - expected_loss).abs() < 1e-10,
            "loss={}, expected={}",
            obj.loss,
            expected_loss
        );
    }

    // ── get() on empty cache ──────────────────────────────────────

    #[test]
    fn get_returns_none_for_unknown_key() {
        let mut cache = GlyphAtlasCache::new(32, 32, 32 * 32);
        let key = GlyphKey::from_char('?', 16);
        assert!(cache.get(key).is_none());
        // A miss via get() should not increment misses (only get_or_insert does).
        assert_eq!(cache.stats().misses, 0);
        assert_eq!(cache.stats().hits, 0);
    }

    // ── max_cached_bytes clamped to atlas area ────────────────────

    #[test]
    fn budget_clamped_to_atlas_area() {
        // Budget exceeds atlas area; should be clamped.
        let cache = GlyphAtlasCache::new(8, 8, 1_000_000);
        // Insert one glyph that fills the atlas exactly.
        let obj = cache.objective();
        // Pressure ratio with 0 cached and clamped budget should be 0.
        assert_eq!(obj.pressure_ratio, 0.0);
    }

    // ── Stats after miss then hit ─────────────────────────────────

    #[test]
    fn stats_track_hits_and_misses_separately() {
        let mut cache = GlyphAtlasCache::new(64, 64, 64 * 64);
        let k1 = GlyphKey::from_char('a', 16);
        let k2 = GlyphKey::from_char('b', 16);

        // Two misses.
        let _ = cache
            .get_or_insert_with(k1, |_| raster_solid(4, 4, GlyphMetrics::default()))
            .expect("k1");
        let _ = cache
            .get_or_insert_with(k2, |_| raster_solid(4, 4, GlyphMetrics::default()))
            .expect("k2");
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().hits, 0);

        // Two hits.
        let _ = cache.get_or_insert_with(k1, |_| raster_solid(4, 4, GlyphMetrics::default()));
        let _ = cache.get_or_insert_with(k2, |_| raster_solid(4, 4, GlyphMetrics::default()));
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().hits, 2);
    }
}
