#![forbid(unsafe_code)]

//! Minimal Roaring Bitmap for cell-level dirty region tracking.
//!
//! Roaring bitmaps efficiently represent sets of integers by adaptively
//! switching between two container types based on density:
//!
//! - **Array container**: Sorted `Vec<u16>` for sparse regions (< 4096 entries).
//! - **Bitmap container**: `[u64; 1024]` (8 KB) for dense regions (>= 4096 entries).
//!
//! Indices are split into `(high16, low16)`: the high 16 bits select the
//! container, the low 16 bits select the position within it.
//!
//! # Usage
//!
//! ```rust
//! use ftui_render::roaring_bitmap::RoaringBitmap;
//!
//! let mut bm = RoaringBitmap::new();
//! bm.insert(42);
//! bm.insert(1000);
//! bm.insert(42); // duplicate is a no-op
//!
//! assert!(bm.contains(42));
//! assert_eq!(bm.cardinality(), 2);
//!
//! let items: Vec<u32> = bm.iter().collect();
//! assert_eq!(items, vec![42, 1000]);
//! ```

/// Threshold at which an array container is promoted to a bitmap container.
const ARRAY_TO_BITMAP_THRESHOLD: usize = 4096;

// ============================================================================
// Container Types
// ============================================================================

/// A sorted array of 16-bit values (sparse container).
#[derive(Clone, Debug)]
struct ArrayContainer {
    values: Vec<u16>,
}

impl ArrayContainer {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn insert(&mut self, value: u16) -> bool {
        match self.values.binary_search(&value) {
            Ok(_) => false, // already present
            Err(pos) => {
                self.values.insert(pos, value);
                true
            }
        }
    }

    fn contains(&self, value: u16) -> bool {
        self.values.binary_search(&value).is_ok()
    }

    fn cardinality(&self) -> usize {
        self.values.len()
    }

    fn should_promote(&self) -> bool {
        self.values.len() >= ARRAY_TO_BITMAP_THRESHOLD
    }

    /// Convert to bitmap container.
    fn to_bitmap(&self) -> BitmapContainer {
        let mut bitmap = BitmapContainer::new();
        for &v in &self.values {
            bitmap.insert(v);
        }
        bitmap
    }
}

/// A fixed-size bitmap of 2^16 bits (dense container).
#[derive(Clone, Debug)]
struct BitmapContainer {
    words: Box<[u64; 1024]>,
    count: usize,
}

impl BitmapContainer {
    fn new() -> Self {
        Self {
            words: Box::new([0u64; 1024]),
            count: 0,
        }
    }

    fn insert(&mut self, value: u16) -> bool {
        let word_idx = (value >> 6) as usize;
        let bit = 1u64 << (value & 63);
        if self.words[word_idx] & bit == 0 {
            self.words[word_idx] |= bit;
            self.count += 1;
            true
        } else {
            false
        }
    }

    fn contains(&self, value: u16) -> bool {
        let word_idx = (value >> 6) as usize;
        let bit = 1u64 << (value & 63);
        self.words[word_idx] & bit != 0
    }

    fn cardinality(&self) -> usize {
        self.count
    }

    fn iter(&self) -> BitmapIter<'_> {
        BitmapIter {
            words: &self.words,
            word_idx: 0,
            current_word: 0,
            started: false,
        }
    }
}

struct BitmapIter<'a> {
    words: &'a [u64; 1024],
    word_idx: usize,
    current_word: u64,
    started: bool,
}

impl Iterator for BitmapIter<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        if !self.started {
            if self.word_idx < 1024 {
                self.current_word = self.words[0];
            }
            self.started = true;
        }

        loop {
            if self.current_word != 0 {
                let bit = self.current_word.trailing_zeros() as u16;
                self.current_word &= self.current_word - 1; // clear lowest set bit
                return Some((self.word_idx as u16) * 64 + bit);
            }
            self.word_idx += 1;
            if self.word_idx >= 1024 {
                return None;
            }
            self.current_word = self.words[self.word_idx];
        }
    }
}

// ============================================================================
// Container Enum
// ============================================================================

#[derive(Clone, Debug)]
enum Container {
    Array(ArrayContainer),
    Bitmap(BitmapContainer),
}

impl Container {
    fn new_array() -> Self {
        Self::Array(ArrayContainer::new())
    }

    fn insert(&mut self, value: u16) -> bool {
        match self {
            Self::Array(arr) => {
                let inserted = arr.insert(value);
                if arr.should_promote() {
                    *self = Self::Bitmap(arr.to_bitmap());
                }
                inserted
            }
            Self::Bitmap(bm) => bm.insert(value),
        }
    }

    fn contains(&self, value: u16) -> bool {
        match self {
            Self::Array(arr) => arr.contains(value),
            Self::Bitmap(bm) => bm.contains(value),
        }
    }

    fn cardinality(&self) -> usize {
        match self {
            Self::Array(arr) => arr.cardinality(),
            Self::Bitmap(bm) => bm.cardinality(),
        }
    }
}

// ============================================================================
// Roaring Bitmap
// ============================================================================

/// Key-container pair, sorted by key.
#[derive(Clone, Debug)]
struct ContainerEntry {
    key: u16,
    container: Container,
}

/// A Roaring Bitmap for efficiently tracking sets of `u32` indices.
///
/// Optimized for the mix of sparse and dense dirty regions typical in
/// terminal UI rendering. Indices are split into (high16, low16) pairs
/// to select containers adaptively.
#[derive(Clone, Debug)]
pub struct RoaringBitmap {
    containers: Vec<ContainerEntry>,
}

impl RoaringBitmap {
    /// Create an empty bitmap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            containers: Vec::new(),
        }
    }

    /// Insert a value into the bitmap. Returns `true` if the value was new.
    pub fn insert(&mut self, value: u32) -> bool {
        let key = (value >> 16) as u16;
        let low = value as u16;

        match self.containers.binary_search_by_key(&key, |e| e.key) {
            Ok(idx) => self.containers[idx].container.insert(low),
            Err(idx) => {
                let mut entry = ContainerEntry {
                    key,
                    container: Container::new_array(),
                };
                entry.container.insert(low);
                self.containers.insert(idx, entry);
                true
            }
        }
    }

    /// Check if the bitmap contains a value.
    #[must_use]
    pub fn contains(&self, value: u32) -> bool {
        let key = (value >> 16) as u16;
        let low = value as u16;

        match self.containers.binary_search_by_key(&key, |e| e.key) {
            Ok(idx) => self.containers[idx].container.contains(low),
            Err(_) => false,
        }
    }

    /// Return the number of values in the bitmap.
    #[must_use]
    pub fn cardinality(&self) -> usize {
        self.containers
            .iter()
            .map(|e| e.container.cardinality())
            .sum()
    }

    /// Check if the bitmap is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.containers
            .iter()
            .all(|e| e.container.cardinality() == 0)
    }

    /// Remove all values.
    pub fn clear(&mut self) {
        self.containers.clear();
    }

    /// Iterate over all values in sorted order.
    pub fn iter(&self) -> RoaringIter<'_> {
        RoaringIter {
            containers: &self.containers,
            container_idx: 0,
            inner: InnerIter::Empty,
        }
    }

    /// Compute the union of two bitmaps.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut result = self.clone();
        for value in other.iter() {
            result.insert(value);
        }
        result
    }

    /// Compute the intersection of two bitmaps.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        let mut result = Self::new();
        // Iterate over the smaller bitmap for efficiency.
        let (smaller, larger) = if self.cardinality() <= other.cardinality() {
            (self, other)
        } else {
            (other, self)
        };
        for value in smaller.iter() {
            if larger.contains(value) {
                result.insert(value);
            }
        }
        result
    }

    /// Insert a range of values `[start, end)`.
    pub fn insert_range(&mut self, start: u32, end: u32) {
        for value in start..end {
            self.insert(value);
        }
    }
}

impl Default for RoaringBitmap {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Iterator
// ============================================================================

enum InnerIter<'a> {
    Empty,
    Array {
        key: u16,
        iter: std::slice::Iter<'a, u16>,
    },
    Bitmap {
        key: u16,
        iter: BitmapIter<'a>,
    },
}

/// Iterator over all values in a [`RoaringBitmap`] in sorted order.
pub struct RoaringIter<'a> {
    containers: &'a [ContainerEntry],
    container_idx: usize,
    inner: InnerIter<'a>,
}

impl Iterator for RoaringIter<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        loop {
            match &mut self.inner {
                InnerIter::Empty => {}
                InnerIter::Array { key, iter } => {
                    if let Some(&low) = iter.next() {
                        return Some(((*key as u32) << 16) | low as u32);
                    }
                }
                InnerIter::Bitmap { key, iter } => {
                    if let Some(low) = iter.next() {
                        return Some(((*key as u32) << 16) | low as u32);
                    }
                }
            }

            // Advance to next container.
            if self.container_idx >= self.containers.len() {
                return None;
            }
            let entry = &self.containers[self.container_idx];
            self.container_idx += 1;

            self.inner = match &entry.container {
                Container::Array(arr) => InnerIter::Array {
                    key: entry.key,
                    iter: arr.values.iter(),
                },
                Container::Bitmap(bm) => InnerIter::Bitmap {
                    key: entry.key,
                    iter: bm.iter(),
                },
            };
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bitmap() {
        let bm = RoaringBitmap::new();
        assert_eq!(bm.cardinality(), 0);
        assert!(bm.is_empty());
        assert!(!bm.contains(0));
        assert_eq!(bm.iter().count(), 0);
    }

    #[test]
    fn insert_and_contains() {
        let mut bm = RoaringBitmap::new();
        assert!(bm.insert(42));
        assert!(!bm.insert(42)); // duplicate
        assert!(bm.contains(42));
        assert!(!bm.contains(43));
        assert_eq!(bm.cardinality(), 1);
    }

    #[test]
    fn insert_multiple_containers() {
        let mut bm = RoaringBitmap::new();
        // Values in different containers (different high 16 bits).
        bm.insert(0);
        bm.insert(65536); // container key = 1
        bm.insert(131072); // container key = 2

        assert_eq!(bm.cardinality(), 3);
        assert!(bm.contains(0));
        assert!(bm.contains(65536));
        assert!(bm.contains(131072));
    }

    #[test]
    fn iteration_order() {
        let mut bm = RoaringBitmap::new();
        bm.insert(100);
        bm.insert(5);
        bm.insert(50);
        bm.insert(1);

        let values: Vec<u32> = bm.iter().collect();
        assert_eq!(values, vec![1, 5, 50, 100]);
    }

    #[test]
    fn iteration_across_containers() {
        let mut bm = RoaringBitmap::new();
        bm.insert(65537); // container 1, value 1
        bm.insert(10); // container 0, value 10
        bm.insert(65536); // container 1, value 0

        let values: Vec<u32> = bm.iter().collect();
        assert_eq!(values, vec![10, 65536, 65537]);
    }

    #[test]
    fn clear() {
        let mut bm = RoaringBitmap::new();
        bm.insert(1);
        bm.insert(2);
        bm.insert(3);
        bm.clear();
        assert_eq!(bm.cardinality(), 0);
        assert!(bm.is_empty());
    }

    #[test]
    fn union_basic() {
        let mut a = RoaringBitmap::new();
        a.insert(1);
        a.insert(3);

        let mut b = RoaringBitmap::new();
        b.insert(2);
        b.insert(3);

        let c = a.union(&b);
        assert_eq!(c.cardinality(), 3);
        assert!(c.contains(1));
        assert!(c.contains(2));
        assert!(c.contains(3));
    }

    #[test]
    fn intersection_basic() {
        let mut a = RoaringBitmap::new();
        a.insert(1);
        a.insert(2);
        a.insert(3);

        let mut b = RoaringBitmap::new();
        b.insert(2);
        b.insert(3);
        b.insert(4);

        let c = a.intersection(&b);
        assert_eq!(c.cardinality(), 2);
        assert!(c.contains(2));
        assert!(c.contains(3));
        assert!(!c.contains(1));
        assert!(!c.contains(4));
    }

    #[test]
    fn intersection_empty() {
        let mut a = RoaringBitmap::new();
        a.insert(1);

        let b = RoaringBitmap::new();
        let c = a.intersection(&b);
        assert!(c.is_empty());
    }

    #[test]
    fn array_to_bitmap_promotion() {
        let mut bm = RoaringBitmap::new();
        // Insert 4096 values to trigger promotion.
        for i in 0..ARRAY_TO_BITMAP_THRESHOLD as u32 {
            bm.insert(i);
        }

        assert_eq!(bm.cardinality(), ARRAY_TO_BITMAP_THRESHOLD);

        // Verify all values are still accessible.
        for i in 0..ARRAY_TO_BITMAP_THRESHOLD as u32 {
            assert!(bm.contains(i), "missing value {i} after promotion");
        }

        // Verify container was promoted to bitmap.
        match &bm.containers[0].container {
            Container::Bitmap(_) => {} // expected
            Container::Array(_) => panic!("container should have been promoted to bitmap"),
        }
    }

    #[test]
    fn cell_index_dirty_tracking() {
        // Simulate a 80x24 terminal with cell-level dirty tracking.
        let width: u32 = 80;
        let _height: u32 = 24;
        let mut dirty = RoaringBitmap::new();

        // Mark some cells dirty.
        let cell = |x: u32, y: u32| -> u32 { y * width + x };

        dirty.insert(cell(0, 0));
        dirty.insert(cell(79, 0)); // last column, first row
        dirty.insert(cell(40, 12)); // middle

        assert_eq!(dirty.cardinality(), 3);
        assert!(dirty.contains(cell(0, 0)));
        assert!(dirty.contains(cell(79, 0)));
        assert!(dirty.contains(cell(40, 12)));
        assert!(!dirty.contains(cell(1, 0)));
    }

    #[test]
    fn large_screen_dirty_tracking() {
        // Simulate a 300x100 terminal.
        let width: u32 = 300;
        let height: u32 = 100;
        let mut dirty = RoaringBitmap::new();

        // Mark an entire row dirty.
        for x in 0..width {
            dirty.insert(10 * width + x);
        }
        assert_eq!(dirty.cardinality(), width as usize);

        // Mark all cells dirty.
        dirty.clear();
        for y in 0..height {
            for x in 0..width {
                dirty.insert(y * width + x);
            }
        }
        assert_eq!(dirty.cardinality(), (width * height) as usize);
    }

    #[test]
    fn insert_range_basic() {
        let mut bm = RoaringBitmap::new();
        bm.insert_range(10, 20);
        assert_eq!(bm.cardinality(), 10);
        for i in 10..20 {
            assert!(bm.contains(i));
        }
        assert!(!bm.contains(9));
        assert!(!bm.contains(20));
    }

    #[test]
    fn union_across_containers() {
        let mut a = RoaringBitmap::new();
        a.insert(100); // container 0

        let mut b = RoaringBitmap::new();
        b.insert(65636); // container 1

        let c = a.union(&b);
        assert_eq!(c.cardinality(), 2);
        assert!(c.contains(100));
        assert!(c.contains(65636));
    }

    #[test]
    fn bitmap_iteration_correctness() {
        let mut bm = BitmapContainer::new();
        bm.insert(0);
        bm.insert(63);
        bm.insert(64);
        bm.insert(65535);

        let values: Vec<u16> = bm.iter().collect();
        assert_eq!(values, vec![0, 63, 64, 65535]);
    }

    #[test]
    fn default_is_empty() {
        let bm = RoaringBitmap::default();
        assert!(bm.is_empty());
    }
}
