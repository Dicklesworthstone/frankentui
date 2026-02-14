#![forbid(unsafe_code)]

//! Read-optimized concurrent stores for read-heavy, write-rare data.
//!
//! Terminal UIs are read-dominated: theme colors, terminal capabilities, and
//! animation clocks are read every frame but changed only on user action or
//! mode switch. A traditional `RwLock` or `Mutex` adds unnecessary contention
//! on the hot read path.
//!
//! This module provides [`ReadOptimized<T>`], a trait abstracting over
//! wait-free read stores, plus three concrete implementations:
//!
//! | Store | Read | Write | Use case |
//! |-------|------|-------|----------|
//! | [`ArcSwapStore`] | wait-free | atomic swap | **Production default** |
//! | [`RwLockStore`] | shared lock | exclusive lock | Baseline comparison |
//! | [`MutexStore`] | exclusive lock | exclusive lock | Baseline comparison |
//!
//! # Constraints
//!
//! - `#![forbid(unsafe_code)]` — all safety delegated to `arc-swap`.
//! - `T: Clone + Send + Sync` — required for cross-thread sharing.
//! - Read path allocates nothing (arc-swap `load` returns a guard, no clone).
//! - Write path allocates one `Arc` per store.
//!
//! # Example
//!
//! ```
//! use ftui_core::read_optimized::{ReadOptimized, ArcSwapStore};
//!
//! let store = ArcSwapStore::new(42u64);
//! assert_eq!(store.load(), 42);
//!
//! store.store(99);
//! assert_eq!(store.load(), 99);
//! ```
//!
//! # Design decision: `arc-swap` over `left-right`
//!
//! Both crates provide safe, lock-free reads. `arc-swap` was chosen because:
//!
//! 1. **Simpler API** — single `ArcSwap<T>` vs read/write handle pairs.
//! 2. **Lower memory** — one `Arc` vs two full copies of `T`.
//! 3. **Sufficient for our types** — `ResolvedTheme` (Copy, 76 bytes) and
//!    `TerminalCapabilities` (Copy, 20 bytes) are tiny; double-buffering
//!    gains nothing.
//! 4. **Zero-dependency unsafe** — our crate stays `#![forbid(unsafe_code)]`;
//!    the unsafe is encapsulated inside `arc-swap`.
//!
//! The `seqlock` crate was rejected because its API requires `unsafe` at the
//! call site.

use std::sync::{Arc, Mutex, RwLock};

use arc_swap::ArcSwap;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A concurrent store optimized for read-heavy access patterns.
///
/// Implementations must guarantee:
/// - `load()` never blocks writers (no writer starvation).
/// - `load()` returns a consistent snapshot (no torn reads).
/// - `store()` is atomic with respect to concurrent `load()` calls.
pub trait ReadOptimized<T: Clone + Send + Sync>: Send + Sync {
    /// Read the current value. Must be wait-free or lock-free.
    fn load(&self) -> T;

    /// Atomically replace the stored value.
    fn store(&self, val: T);
}

// ---------------------------------------------------------------------------
// ArcSwapStore — production default
// ---------------------------------------------------------------------------

/// Wait-free reads via [`arc_swap::ArcSwap`].
///
/// - `load()`: wait-free, returns cloned `T` from a guard (no allocation).
/// - `store()`: allocates one `Arc`, atomically swaps.
///
/// Best for: read-99%/write-1% data like themes and capabilities.
pub struct ArcSwapStore<T> {
    inner: ArcSwap<T>,
}

impl<T: Clone + Send + Sync> ArcSwapStore<T> {
    /// Create a new store with an initial value.
    pub fn new(val: T) -> Self {
        Self {
            inner: ArcSwap::from_pointee(val),
        }
    }

    /// Read without cloning — returns a guard that derefs to `T`.
    ///
    /// Prefer this when you only need a short-lived reference.
    pub fn load_ref(&self) -> arc_swap::Guard<Arc<T>> {
        self.inner.load()
    }
}

impl<T: Clone + Send + Sync> ReadOptimized<T> for ArcSwapStore<T> {
    #[inline]
    fn load(&self) -> T {
        // Guard derefs to Arc<T>, clone T out.
        let guard = self.inner.load();
        T::clone(&guard)
    }

    #[inline]
    fn store(&self, val: T) {
        self.inner.store(Arc::new(val));
    }
}

// ---------------------------------------------------------------------------
// RwLockStore — baseline comparison
// ---------------------------------------------------------------------------

/// Shared-lock reads via [`std::sync::RwLock`].
///
/// Included for benchmark comparison; prefer [`ArcSwapStore`] in production.
pub struct RwLockStore<T> {
    inner: RwLock<T>,
}

impl<T: Clone + Send + Sync> RwLockStore<T> {
    /// Create a new store with an initial value.
    pub fn new(val: T) -> Self {
        Self {
            inner: RwLock::new(val),
        }
    }
}

impl<T: Clone + Send + Sync> ReadOptimized<T> for RwLockStore<T> {
    #[inline]
    fn load(&self) -> T {
        self.inner.read().expect("RwLock poisoned").clone()
    }

    #[inline]
    fn store(&self, val: T) {
        *self.inner.write().expect("RwLock poisoned") = val;
    }
}

// ---------------------------------------------------------------------------
// MutexStore — baseline comparison
// ---------------------------------------------------------------------------

/// Exclusive-lock reads via [`std::sync::Mutex`].
///
/// Included for benchmark comparison; prefer [`ArcSwapStore`] in production.
pub struct MutexStore<T> {
    inner: Mutex<T>,
}

impl<T: Clone + Send + Sync> MutexStore<T> {
    /// Create a new store with an initial value.
    pub fn new(val: T) -> Self {
        Self {
            inner: Mutex::new(val),
        }
    }
}

impl<T: Clone + Send + Sync> ReadOptimized<T> for MutexStore<T> {
    #[inline]
    fn load(&self) -> T {
        self.inner.lock().expect("Mutex poisoned").clone()
    }

    #[inline]
    fn store(&self, val: T) {
        *self.inner.lock().expect("Mutex poisoned") = val;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    // -- Helpers -----------------------------------------------------------

    /// A non-Copy type to exercise the Clone path.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Config {
        name: String,
        value: u64,
    }

    fn make_config(n: u64) -> Config {
        Config {
            name: format!("cfg-{n}"),
            value: n,
        }
    }

    // -- ArcSwapStore tests -----------------------------------------------

    #[test]
    fn arcswap_load_returns_initial_value() {
        let store = ArcSwapStore::new(42u64);
        assert_eq!(store.load(), 42);
    }

    #[test]
    fn arcswap_store_then_load() {
        let store = ArcSwapStore::new(0u64);
        store.store(99);
        assert_eq!(store.load(), 99);
    }

    #[test]
    fn arcswap_load_ref_borrows_without_clone() {
        let store = ArcSwapStore::new(make_config(1));
        let guard = store.load_ref();
        assert_eq!(guard.name, "cfg-1");
        assert_eq!(guard.value, 1);
    }

    #[test]
    fn arcswap_multiple_stores_last_wins() {
        let store = ArcSwapStore::new(0u64);
        for i in 1..=100 {
            store.store(i);
        }
        assert_eq!(store.load(), 100);
    }

    #[test]
    fn arcswap_concurrent_reads_never_panic() {
        let store = Arc::new(ArcSwapStore::new(make_config(0)));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = Arc::clone(&store);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    for _ in 0..1000 {
                        let _ = s.load();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn arcswap_concurrent_read_write() {
        let store = Arc::new(ArcSwapStore::new(0u64));
        let barrier = Arc::new(Barrier::new(9)); // 8 readers + 1 writer

        // 8 reader threads
        let readers: Vec<_> = (0..8)
            .map(|_| {
                let s = Arc::clone(&store);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    let mut last = 0u64;
                    for _ in 0..10_000 {
                        let v = s.load();
                        // Values must be monotonically non-decreasing
                        // (single writer increments sequentially).
                        assert!(v >= last, "stale read: got {v}, expected >= {last}");
                        last = v;
                    }
                })
            })
            .collect();

        // 1 writer thread
        let writer = {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                for i in 1..=10_000u64 {
                    s.store(i);
                }
            })
        };

        writer.join().unwrap();
        for h in readers {
            h.join().unwrap();
        }
        assert_eq!(store.load(), 10_000);
    }

    // -- RwLockStore tests ------------------------------------------------

    #[test]
    fn rwlock_load_returns_initial_value() {
        let store = RwLockStore::new(42u64);
        assert_eq!(store.load(), 42);
    }

    #[test]
    fn rwlock_store_then_load() {
        let store = RwLockStore::new(0u64);
        store.store(99);
        assert_eq!(store.load(), 99);
    }

    #[test]
    fn rwlock_concurrent_read_write() {
        let store = Arc::new(RwLockStore::new(0u64));
        let barrier = Arc::new(Barrier::new(5));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let s = Arc::clone(&store);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    for _ in 0..5_000 {
                        let _ = s.load();
                    }
                })
            })
            .collect();

        let writer = {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                for i in 1..=5_000u64 {
                    s.store(i);
                }
            })
        };

        writer.join().unwrap();
        for h in readers {
            h.join().unwrap();
        }
        assert_eq!(store.load(), 5_000);
    }

    // -- MutexStore tests -------------------------------------------------

    #[test]
    fn mutex_load_returns_initial_value() {
        let store = MutexStore::new(42u64);
        assert_eq!(store.load(), 42);
    }

    #[test]
    fn mutex_store_then_load() {
        let store = MutexStore::new(0u64);
        store.store(99);
        assert_eq!(store.load(), 99);
    }

    #[test]
    fn mutex_concurrent_read_write() {
        let store = Arc::new(MutexStore::new(0u64));
        let barrier = Arc::new(Barrier::new(5));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let s = Arc::clone(&store);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    for _ in 0..5_000 {
                        let _ = s.load();
                    }
                })
            })
            .collect();

        let writer = {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                for i in 1..=5_000u64 {
                    s.store(i);
                }
            })
        };

        writer.join().unwrap();
        for h in readers {
            h.join().unwrap();
        }
        assert_eq!(store.load(), 5_000);
    }

    // -- Trait object tests -----------------------------------------------

    #[test]
    fn trait_object_arcswap() {
        let store: Box<dyn ReadOptimized<u64>> = Box::new(ArcSwapStore::new(10));
        assert_eq!(store.load(), 10);
        store.store(20);
        assert_eq!(store.load(), 20);
    }

    #[test]
    fn trait_object_rwlock() {
        let store: Box<dyn ReadOptimized<u64>> = Box::new(RwLockStore::new(10));
        assert_eq!(store.load(), 10);
        store.store(20);
        assert_eq!(store.load(), 20);
    }

    #[test]
    fn trait_object_mutex() {
        let store: Box<dyn ReadOptimized<u64>> = Box::new(MutexStore::new(10));
        assert_eq!(store.load(), 10);
        store.store(20);
        assert_eq!(store.load(), 20);
    }

    // -- Copy type tests (simulating ResolvedTheme / TerminalCapabilities) -

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeCaps {
        true_color: bool,
        sync_output: bool,
        mouse_sgr: bool,
    }

    #[test]
    fn arcswap_with_copy_type() {
        let caps = FakeCaps {
            true_color: true,
            sync_output: false,
            mouse_sgr: true,
        };
        let store = ArcSwapStore::new(caps);
        assert_eq!(store.load(), caps);

        let updated = FakeCaps {
            true_color: true,
            sync_output: true,
            mouse_sgr: true,
        };
        store.store(updated);
        assert_eq!(store.load(), updated);
    }

    #[test]
    fn concurrent_copy_type_reads() {
        let caps = FakeCaps {
            true_color: true,
            sync_output: false,
            mouse_sgr: true,
        };
        let store = Arc::new(ArcSwapStore::new(caps));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = Arc::clone(&store);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    for _ in 0..10_000 {
                        let v = s.load();
                        // Value must be one of the two valid states.
                        assert!(v.true_color);
                        assert!(v.mouse_sgr);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
