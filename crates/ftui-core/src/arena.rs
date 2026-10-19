#![forbid(unsafe_code)]

//! Per-frame bump arena allocation.
//!
//! Provides [`FrameArena`], a thin wrapper around [`bumpalo::Bump`] for
//! per-frame temporary allocations. The arena is reset at frame boundaries,
//! eliminating allocator churn on the hot render path.
//!
//! # Usage
//!
//! ```
//! use ftui_core::arena::FrameArena;
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

/// A growable UTF-8 string allocated in the bump arena.
///
/// The memory is reclaimed when the arena is reset at frame boundaries.
pub type BumpString<'a> = bumpalo::collections::String<'a>;

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

    /// Create a new growable string backed by this arena.
    pub fn new_string(&self) -> BumpString<'_> {
        bumpalo::collections::String::new_in(&self.bump)
    }

    /// Create a new growable string with the given capacity.
    pub fn new_string_with_capacity(&self, capacity: usize) -> BumpString<'_> {
        bumpalo::collections::String::with_capacity_in(capacity, &self.bump)
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

    #[test]
    fn core_arena_alloc_and_reset() {
        let mut arena = FrameArena::new(4096);
        let s = arena.alloc_str("ftui-core arena");
        assert_eq!(s, "ftui-core arena");
        let v = arena.alloc_slice(&[10u32, 20, 30]);
        assert_eq!(v, &[10, 20, 30]);
        arena.reset();
        let s2 = arena.alloc_str("reused");
        assert_eq!(s2, "reused");
    }
}
