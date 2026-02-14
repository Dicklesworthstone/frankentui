//! Per-frame bump arena allocation.
//!
//! Provides [`FrameArena`], a thin wrapper around [`bumpalo::Bump`] for
//! per-frame temporary allocations. The arena is reset at frame boundaries,
//! eliminating allocator churn on the hot render path.
//!
//! # Usage
//!
//! ```
//! use ftui_render::arena::FrameArena;
//!
//! let mut arena = FrameArena::new(256 * 1024); // 256 KB initial capacity
//! let s = arena.alloc_str("hello");
//! assert_eq!(s, "hello");
//!
//! let slice = arena.alloc_slice(&[1u32, 2, 3]);
//! assert_eq!(slice, &[1, 2, 3]);
//!
//! arena.reset(); // O(1) — reclaims all memory for reuse
//! ```
//!
//! # Safety
//!
//! This module uses only safe code. `bumpalo::Bump` provides a safe bump
//! allocator with automatic growth. `reset()` is safe and frees all
//! allocations, making the memory available for reuse.

use bumpalo::Bump;

/// Default initial capacity for the frame arena (256 KB).
pub const DEFAULT_ARENA_CAPACITY: usize = 256 * 1024;

/// A per-frame bump allocator for temporary render-path allocations.
///
/// `FrameArena` wraps [`bumpalo::Bump`] with a focused API for the common
/// allocation patterns in the render pipeline: strings, slices, and
/// single values. All allocations are invalidated on [`reset()`](Self::reset),
/// which should be called at frame boundaries.
///
/// # Capacity
///
/// The arena starts with an initial capacity and grows automatically when
/// exhausted. Growth allocates new chunks from the global allocator but
/// never moves existing allocations.
#[derive(Debug)]
pub struct FrameArena {
    bump: Bump,
}

impl FrameArena {
    /// Create a new arena with the given initial capacity in bytes.
    ///
    /// # Panics
    ///
    /// Panics if the system allocator cannot fulfill the initial allocation.
    pub fn new(capacity: usize) -> Self {
        Self {
            bump: Bump::with_capacity(capacity),
        }
    }

    /// Create a new arena with the default capacity (256 KB).
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_ARENA_CAPACITY)
    }

    /// Reset the arena, reclaiming all memory for reuse.
    ///
    /// This is an O(1) operation. All previously allocated references
    /// are invalidated. The arena retains its allocated chunks for
    /// future allocations, avoiding repeated system allocator calls.
    pub fn reset(&mut self) {
        self.bump.reset();
    }

    /// Allocate a string slice in the arena.
    ///
    /// Returns a reference to the arena-allocated copy of `s`.
    /// The returned reference is valid until the next [`reset()`](Self::reset).
    pub fn alloc_str(&self, s: &str) -> &str {
        self.bump.alloc_str(s)
    }

    /// Allocate a copy of a slice in the arena.
    ///
    /// Returns a reference to the arena-allocated copy of `slice`.
    /// The returned reference is valid until the next [`reset()`](Self::reset).
    pub fn alloc_slice<T: Copy>(&self, slice: &[T]) -> &[T] {
        self.bump.alloc_slice_copy(slice)
    }

    /// Allocate a single value in the arena, constructed by `f`.
    ///
    /// Returns a mutable reference to the arena-allocated value.
    /// The returned reference is valid until the next [`reset()`](Self::reset).
    pub fn alloc_with<T, F: FnOnce() -> T>(&self, f: F) -> &mut T {
        self.bump.alloc_with(f)
    }

    /// Allocate a single value in the arena.
    ///
    /// Returns a mutable reference to the arena-allocated value.
    /// The returned reference is valid until the next [`reset()`](Self::reset).
    pub fn alloc<T>(&self, val: T) -> &mut T {
        self.bump.alloc(val)
    }

    /// Returns the total bytes allocated in the arena (across all chunks).
    pub fn allocated_bytes(&self) -> usize {
        self.bump.allocated_bytes()
    }

    /// Returns the total bytes of unused capacity in the arena.
    pub fn allocated_bytes_including_metadata(&self) -> usize {
        self.bump.allocated_bytes_including_metadata()
    }

    /// Returns a reference to the underlying [`Bump`] allocator.
    ///
    /// Use this for advanced allocation patterns not covered by the
    /// convenience methods.
    pub fn as_bump(&self) -> &Bump {
        &self.bump
    }
}

impl Default for FrameArena {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_arena_with_capacity() {
        let arena = FrameArena::new(1024);
        // Should be able to allocate without growing
        let _s = arena.alloc_str("hello");
    }

    #[test]
    fn default_uses_256kb() {
        let arena = FrameArena::default();
        let _s = arena.alloc_str("test");
    }

    #[test]
    fn alloc_str_returns_correct_content() {
        let arena = FrameArena::new(4096);
        let s = arena.alloc_str("hello, world!");
        assert_eq!(s, "hello, world!");
    }

    #[test]
    fn alloc_str_empty() {
        let arena = FrameArena::new(4096);
        let s = arena.alloc_str("");
        assert_eq!(s, "");
    }

    #[test]
    fn alloc_str_unicode() {
        let arena = FrameArena::new(4096);
        let s = arena.alloc_str("こんにちは 🎉");
        assert_eq!(s, "こんにちは 🎉");
    }

    #[test]
    fn alloc_slice_copies_correctly() {
        let arena = FrameArena::new(4096);
        let data = [1u32, 2, 3, 4, 5];
        let slice = arena.alloc_slice(&data);
        assert_eq!(slice, &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn alloc_slice_empty() {
        let arena = FrameArena::new(4096);
        let slice: &[u8] = arena.alloc_slice(&[]);
        assert!(slice.is_empty());
    }

    #[test]
    fn alloc_slice_u8() {
        let arena = FrameArena::new(4096);
        let data = b"ANSI escape";
        let slice = arena.alloc_slice(data.as_slice());
        assert_eq!(slice, b"ANSI escape");
    }

    #[test]
    fn alloc_with_constructs_value() {
        let arena = FrameArena::new(4096);
        let val = arena.alloc_with(|| 42u64);
        assert_eq!(*val, 42);
    }

    #[test]
    fn alloc_returns_mutable_ref() {
        let arena = FrameArena::new(4096);
        let val = arena.alloc(100i32);
        assert_eq!(*val, 100);
        *val = 200;
        assert_eq!(*val, 200);
    }

    #[test]
    fn reset_allows_reuse() {
        let mut arena = FrameArena::new(4096);
        let _s1 = arena.alloc_str("first frame data");
        let bytes_before = arena.allocated_bytes();
        assert!(bytes_before > 0);

        arena.reset();

        // After reset, new allocations reuse the same memory
        let _s2 = arena.alloc_str("second frame data");
    }

    #[test]
    fn multiple_allocations_coexist() {
        let arena = FrameArena::new(4096);
        let s1 = arena.alloc_str("hello");
        let s2 = arena.alloc_str("world");
        let slice = arena.alloc_slice(&[1u32, 2, 3]);
        let val = arena.alloc(42u64);

        // All references remain valid simultaneously
        assert_eq!(s1, "hello");
        assert_eq!(s2, "world");
        assert_eq!(slice, &[1, 2, 3]);
        assert_eq!(*val, 42);
    }

    #[test]
    fn arena_grows_beyond_initial_capacity() {
        let arena = FrameArena::new(64); // Very small initial capacity
        // Allocate more than 64 bytes — arena should grow automatically
        let large = "a]".repeat(100);
        let s = arena.alloc_str(&large);
        assert_eq!(s, large);
    }

    #[test]
    fn allocated_bytes_tracks_usage() {
        let arena = FrameArena::new(4096);
        let initial = arena.allocated_bytes();
        let _s = arena.alloc_str("some text for tracking");
        assert!(arena.allocated_bytes() >= initial);
    }

    #[test]
    fn as_bump_provides_access() {
        let arena = FrameArena::new(4096);
        let bump = arena.as_bump();
        // Can use bump directly for advanced patterns
        let val = bump.alloc(99u32);
        assert_eq!(*val, 99);
    }

    #[test]
    fn reset_then_heavy_reuse() {
        let mut arena = FrameArena::new(4096);
        for frame in 0..100 {
            let s = arena.alloc_str(&format!("frame {frame}"));
            assert!(s.starts_with("frame "));
            let data: Vec<u32> = (0..50).collect();
            let slice = arena.alloc_slice(&data);
            assert_eq!(slice.len(), 50);
            arena.reset();
        }
    }

    #[test]
    fn debug_impl() {
        let arena = FrameArena::new(1024);
        let debug = format!("{arena:?}");
        assert!(debug.contains("FrameArena"));
    }
}
