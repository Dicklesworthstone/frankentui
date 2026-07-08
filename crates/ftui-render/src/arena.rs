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

/// A growable vector allocated in the bump arena.
///
/// Use this for scratch collections that need `push()` during rendering.
/// The memory is reclaimed when the arena is reset at frame boundaries.
pub type BumpVec<'a, T> = bumpalo::collections::Vec<'a, T>;

/// Default initial capacity for the frame arena (256 KB).
pub const DEFAULT_ARENA_CAPACITY: usize = 256 * 1024;

/// A per-frame bump allocator for temporary render-path allocations.
///
/// `FrameArena` wraps [`bumpalo::Bump`] with a focused API for the common
/// allocation patterns in the render pipeline: strings, slices, and
/// single values. All allocations are invalidated on [`reset()`](Self::reset),
/// which should be called at frame boundaries.
///
/// # Drop semantics
///
/// `bumpalo` intentionally does not run `Drop` for values allocated in the arena
/// when calling [`reset()`](Self::reset) or when the arena itself is dropped.
/// Only allocate short-lived scratch values that do not require destructor logic.
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

    /// Format a string directly into the arena, avoiding an intermediate heap `String`.
    ///
    /// This is the arena equivalent of `format!()`. The formatted text is
    /// written into a bump-backed string and returned as an arena-allocated `&str`.
    pub fn alloc_fmt(&self, args: std::fmt::Arguments<'_>) -> &str {
        use core::fmt::Write;
        let mut s = bumpalo::collections::String::new_in(&self.bump);
        s.write_fmt(args).expect("formatting into arena string");
        s.into_bump_str()
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
    ///
    /// NOTE: destructors are NEVER run for bump allocations — neither on
    /// `reset()` nor on arena drop (pinned by test). Allocating a `Drop`
    /// type (Rc, String, Vec, file handles…) leaks whatever the destructor
    /// would have released; use arena allocation for plain data only.
    pub fn alloc<T>(&self, val: T) -> &mut T {
        self.bump.alloc(val)
    }

    /// Collect an iterator into an arena-allocated slice.
    ///
    /// This is the arena equivalent of `.collect::<Vec<T>>()` — it avoids
    /// a heap allocation by writing elements directly into bump memory.
    pub fn alloc_iter<T, I>(&self, iter: I) -> &mut [T]
    where
        I: IntoIterator<Item = T>,
    {
        let mut vec = bumpalo::collections::Vec::new_in(&self.bump);
        vec.extend(iter);
        vec.into_bump_slice_mut()
    }

    /// Create a new growable vector backed by this arena.
    ///
    /// Use this for scratch collections that grow via `push()` during
    /// rendering. The vector's memory is reclaimed on arena reset.
    pub fn new_vec<T>(&self) -> BumpVec<'_, T> {
        bumpalo::collections::Vec::new_in(&self.bump)
    }

    /// Create a new growable vector with the given capacity.
    pub fn new_vec_with_capacity<T>(&self, capacity: usize) -> BumpVec<'_, T> {
        bumpalo::collections::Vec::with_capacity_in(capacity, &self.bump)
    }

    /// Returns the total bytes allocated in the arena (across all chunks).
    ///
    /// IMPORTANT: after [`reset()`](Self::reset) this reports the RETAINED
    /// chunk capacity (bumpalo keeps its largest chunk for reuse), not live
    /// allocation usage — it never decreases on its own. Treating it as a
    /// "current usage" signal creates ratchet-and-stick behavior: this
    /// exact misreading made the frame guardrails' emergency verdict
    /// permanent (fixed by rebuilding the arena on the drop path). To
    /// actually release memory, drop and recreate the arena.
    pub fn allocated_bytes(&self) -> usize {
        self.bump.allocated_bytes()
    }

    /// Returns total allocated bytes including allocator metadata.
    ///
    /// This reflects chunk footprint, not currently live allocation usage.
    /// Chunk memory is retained across [`reset()`](Self::reset) for reuse.
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
    use proptest::prelude::*;
    use std::cell::Cell as DropCounter;
    use std::mem::align_of;
    use std::rc::Rc;

    #[derive(Clone)]
    struct DropSpy {
        drops: Rc<DropCounter<usize>>,
    }

    impl Drop for DropSpy {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn alloc_fmt_formats_into_arena() {
        let arena = FrameArena::new(4096);
        let s = arena.alloc_fmt(format_args!("hello {} {}", 42, "world"));
        assert_eq!(s, "hello 42 world");
    }

    #[test]
    fn alloc_iter_collects_to_arena() {
        let arena = FrameArena::new(4096);
        let data: Vec<u32> = (0..10).collect();
        let slice = arena.alloc_iter(data.iter().copied());
        assert_eq!(slice, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn alloc_iter_empty() {
        let arena = FrameArena::new(4096);
        let slice: &mut [u8] = arena.alloc_iter(std::iter::empty());
        assert!(slice.is_empty());
    }

    #[test]
    fn new_vec_push_and_read() {
        let arena = FrameArena::new(4096);
        let mut v = arena.new_vec::<u32>();
        v.push(1);
        v.push(2);
        v.push(3);
        assert_eq!(v.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn new_vec_with_capacity_preallocates() {
        let arena = FrameArena::new(4096);
        let v = arena.new_vec_with_capacity::<u64>(100);
        assert!(v.capacity() >= 100);
        assert!(v.is_empty());
    }

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
    fn default_capacity_grows_beyond_256kb_without_panic() {
        let arena = FrameArena::default();
        let large = vec![0xAB; DEFAULT_ARENA_CAPACITY + 64 * 1024];
        let s = arena.alloc_slice(&large);
        assert_eq!(s.len(), large.len());
        assert_eq!(s[0], 0xAB);
        assert!(arena.allocated_bytes() >= DEFAULT_ARENA_CAPACITY);
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
    fn allocations_respect_alignment_requirements() {
        let arena = FrameArena::new(4096);

        let p_u8 = arena.alloc(1u8) as *mut u8 as usize;
        let p_u32 = arena.alloc(2u32) as *mut u32 as usize;
        let p_u64 = arena.alloc(3u64) as *mut u64 as usize;
        let p_u128 = arena.alloc(4u128) as *mut u128 as usize;

        assert_eq!(p_u8 % align_of::<u8>(), 0);
        assert_eq!(p_u32 % align_of::<u32>(), 0);
        assert_eq!(p_u64 % align_of::<u64>(), 0);
        assert_eq!(p_u128 % align_of::<u128>(), 0);
    }

    #[test]
    fn reset_reuses_existing_chunks_without_extra_growth() {
        let mut arena = FrameArena::new(128);
        let payload = vec![7u8; 32 * 1024];

        let first = arena.alloc_slice(&payload);
        assert_eq!(first.len(), payload.len());
        let grown = arena.allocated_bytes_including_metadata();
        assert!(grown > 128);

        arena.reset();

        let second = arena.alloc_slice(&payload);
        assert_eq!(second.len(), payload.len());
        let after = arena.allocated_bytes_including_metadata();
        assert!(
            after <= grown + 1024,
            "arena should reuse existing chunks after reset: before={grown}, after={after}"
        );
    }

    #[test]
    fn reset_does_not_run_drop_glue_for_allocated_values() {
        let drops = Rc::new(DropCounter::new(0));
        {
            let mut arena = FrameArena::new(1024);
            let _spy = arena.alloc(DropSpy {
                drops: Rc::clone(&drops),
            });
            arena.reset();
            assert_eq!(
                drops.get(),
                0,
                "reset() must not run Drop for bump allocations"
            );
        }
        assert_eq!(
            drops.get(),
            0,
            "dropping arena must not run Drop for bump allocations"
        );
    }

    #[test]
    fn debug_impl() {
        let arena = FrameArena::new(1024);
        let debug = format!("{arena:?}");
        assert!(debug.contains("FrameArena"));
    }

    proptest! {
        #[test]
        fn proptest_random_alloc_reset_sequences_never_panic(ops in prop::collection::vec((0u8..=3, 0u16..1024), 1..300)) {
            let mut arena = FrameArena::new(256);
            for (op, size_hint) in ops {
                match op {
                    0 => {
                        let len = (size_hint as usize % 256) + 1;
                        let s = "x".repeat(len);
                        let alloc = arena.alloc_str(&s);
                        prop_assert_eq!(alloc.len(), len);
                    }
                    1 => {
                        let len = (size_hint as usize % 128) + 1;
                        let data = vec![size_hint as u32; len];
                        let alloc = arena.alloc_slice(&data);
                        prop_assert_eq!(alloc.len(), len);
                    }
                    2 => {
                        let value = arena.alloc(size_hint as u64);
                        prop_assert_eq!(*value, size_hint as u64);
                    }
                    _ => {
                        arena.reset();
                    }
                }
            }
        }
    }
}
