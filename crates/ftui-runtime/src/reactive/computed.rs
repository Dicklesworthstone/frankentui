#![forbid(unsafe_code)]

//! Lazy computed values that auto-update from [`Observable`] dependencies.
//!
//! # Design
//!
//! [`Computed<T>`] wraps a compute function and its cached result in shared,
//! reference-counted storage. When any dependency changes, the cached value is
//! invalidated (marked dirty). The next call to [`get()`](Computed::get)
//! recomputes and caches the result.
//!
//! # Invariants
//!
//! 1. `get()` always returns a value consistent with the current state of all
//!    dependencies (no stale reads after a dependency mutation completes).
//! 2. The compute function is called at most once per dependency change cycle
//!    (memoization).
//! 3. If no dependency has changed, `get()` returns the cached value in O(1).
//! 4. Version increments by exactly 1 per recomputation.
//!
//! # Failure Modes
//!
//! - **Compute function panics**: The cached value remains from the last
//!   successful computation. The dirty flag stays set so the next `get()` will
//!   retry.
//! - **Dependency dropped**: If the source `Observable` is dropped, the
//!   subscription becomes inert. The computed value retains its last cached
//!   result and never becomes dirty again from that source.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::observable::{Observable, Subscription};

/// Shared interior for [`Computed<T>`].
struct ComputedInner<T> {
    /// The computation function.
    compute: Box<dyn Fn() -> T>,
    /// Cached result (None only before first computation).
    cached: Option<T>,
    /// Whether the cached value is stale.
    dirty: Cell<bool>,
    /// Monotonically increasing version, bumped on each recomputation.
    version: u64,
    /// Subscription guards keeping dependency callbacks alive.
    /// These are never read after construction, but must be kept alive.
    _subscriptions: Vec<Subscription>,
}

/// A lazily-evaluated, memoized value derived from one or more [`Observable`]
/// dependencies.
///
/// Cloning a `Computed` creates a new handle to the **same** inner state.
///
/// # Invariants
///
/// 1. `dirty` is true after any dependency changes and before `get()`.
/// 2. `version` increments by 1 on each recomputation.
/// 3. The compute function is called only when `dirty` is true and `get()`
///    is called.
pub struct Computed<T> {
    inner: Rc<RefCell<ComputedInner<T>>>,
}

impl<T> Clone for Computed<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Computed<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.borrow();
        f.debug_struct("Computed")
            .field("cached", &inner.cached)
            .field("dirty", &inner.dirty.get())
            .field("version", &inner.version)
            .finish()
    }
}

impl<T: Clone + 'static> Computed<T> {
    /// Create a computed value derived from a single observable.
    ///
    /// The `map` function receives a reference to the source value and
    /// returns the derived value.
    pub fn from_observable<S: Clone + PartialEq + 'static>(
        source: &Observable<S>,
        map: impl Fn(&S) -> T + 'static,
    ) -> Self {
        let source_clone = source.clone();
        let compute = Box::new(move || source_clone.with(|v| map(v)));

        let inner = Rc::new(RefCell::new(ComputedInner {
            compute,
            cached: None,
            dirty: Cell::new(true), // Dirty initially — will compute on first get().
            version: 0,
            _subscriptions: Vec::new(),
        }));

        // Subscribe to the source to mark dirty on change.
        let weak_inner = Rc::downgrade(&inner);
        let sub = source.subscribe(move |_| {
            if let Some(strong) = weak_inner.upgrade() {
                strong.borrow().dirty.set(true);
            }
        });

        inner.borrow_mut()._subscriptions.push(sub);

        Self { inner }
    }

    /// Create a computed value derived from two observables.
    pub fn from2<S1, S2>(
        s1: &Observable<S1>,
        s2: &Observable<S2>,
        map: impl Fn(&S1, &S2) -> T + 'static,
    ) -> Self
    where
        S1: Clone + PartialEq + 'static,
        S2: Clone + PartialEq + 'static,
    {
        let s1_clone = s1.clone();
        let s2_clone = s2.clone();
        let compute = Box::new(move || s1_clone.with(|v1| s2_clone.with(|v2| map(v1, v2))));

        let inner = Rc::new(RefCell::new(ComputedInner {
            compute,
            cached: None,
            dirty: Cell::new(true),
            version: 0,
            _subscriptions: Vec::new(),
        }));

        // Subscribe to both sources.
        let weak1 = Rc::downgrade(&inner);
        let sub1 = s1.subscribe(move |_| {
            if let Some(strong) = weak1.upgrade() {
                strong.borrow().dirty.set(true);
            }
        });

        let weak2 = Rc::downgrade(&inner);
        let sub2 = s2.subscribe(move |_| {
            if let Some(strong) = weak2.upgrade() {
                strong.borrow().dirty.set(true);
            }
        });

        {
            let mut inner_mut = inner.borrow_mut();
            inner_mut._subscriptions.push(sub1);
            inner_mut._subscriptions.push(sub2);
        }

        Self { inner }
    }

    /// Create a computed value derived from three observables.
    pub fn from3<S1, S2, S3>(
        s1: &Observable<S1>,
        s2: &Observable<S2>,
        s3: &Observable<S3>,
        map: impl Fn(&S1, &S2, &S3) -> T + 'static,
    ) -> Self
    where
        S1: Clone + PartialEq + 'static,
        S2: Clone + PartialEq + 'static,
        S3: Clone + PartialEq + 'static,
    {
        let s1_clone = s1.clone();
        let s2_clone = s2.clone();
        let s3_clone = s3.clone();
        let compute = Box::new(move || {
            s1_clone.with(|v1| s2_clone.with(|v2| s3_clone.with(|v3| map(v1, v2, v3))))
        });

        let inner = Rc::new(RefCell::new(ComputedInner {
            compute,
            cached: None,
            dirty: Cell::new(true),
            version: 0,
            _subscriptions: Vec::new(),
        }));

        let weak1 = Rc::downgrade(&inner);
        let sub1 = s1.subscribe(move |_| {
            if let Some(strong) = weak1.upgrade() {
                strong.borrow().dirty.set(true);
            }
        });

        let weak2 = Rc::downgrade(&inner);
        let sub2 = s2.subscribe(move |_| {
            if let Some(strong) = weak2.upgrade() {
                strong.borrow().dirty.set(true);
            }
        });

        let weak3 = Rc::downgrade(&inner);
        let sub3 = s3.subscribe(move |_| {
            if let Some(strong) = weak3.upgrade() {
                strong.borrow().dirty.set(true);
            }
        });

        {
            let mut inner_mut = inner.borrow_mut();
            inner_mut._subscriptions.push(sub1);
            inner_mut._subscriptions.push(sub2);
            inner_mut._subscriptions.push(sub3);
        }

        Self { inner }
    }

    /// Create a computed value from a standalone compute function and
    /// pre-built subscriptions.
    ///
    /// This is the low-level constructor for advanced use cases where
    /// the caller manages dependency subscriptions manually.
    pub fn from_fn(compute: impl Fn() -> T + 'static, subscriptions: Vec<Subscription>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(ComputedInner {
                compute: Box::new(compute),
                cached: None,
                dirty: Cell::new(true),
                version: 0,
                _subscriptions: subscriptions,
            })),
        }
    }

    /// Get the current value, recomputing if any dependency has changed.
    ///
    /// Returns a clone of the cached value. If the value is dirty, the
    /// compute function is called first and the result is cached.
    #[must_use]
    pub fn get(&self) -> T {
        let mut inner = self.inner.borrow_mut();
        if inner.dirty.get() || inner.cached.is_none() {
            let new_value = (inner.compute)();
            inner.cached = Some(new_value);
            inner.dirty.set(false);
            inner.version += 1;
        }
        inner
            .cached
            .as_ref()
            .expect("cached is always Some after get()")
            .clone()
    }

    /// Access the current value by reference without cloning.
    ///
    /// Forces recomputation if dirty. The closure receives an immutable
    /// reference to the cached value.
    ///
    /// # Panics
    ///
    /// Panics if the closure attempts to call `get()` on the same
    /// `Computed` (re-entrant borrow).
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        // Ensure the value is fresh.
        {
            let mut inner = self.inner.borrow_mut();
            if inner.dirty.get() || inner.cached.is_none() {
                let new_value = (inner.compute)();
                inner.cached = Some(new_value);
                inner.dirty.set(false);
                inner.version += 1;
            }
        }
        let inner = self.inner.borrow();
        f(inner
            .cached
            .as_ref()
            .expect("cached is always Some after refresh"))
    }

    /// Whether the cached value is stale.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.inner.borrow().dirty.get()
    }

    /// Force invalidation of the cached value. The next `get()` will
    /// recompute.
    pub fn invalidate(&self) {
        self.inner.borrow().dirty.set(true);
    }

    /// Current version number. Increments by 1 on each recomputation.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.borrow().version
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn single_dep_computed() {
        let source = Observable::new(10);
        let computed = Computed::from_observable(&source, |v| v * 2);

        assert_eq!(computed.get(), 20);
        assert_eq!(computed.version(), 1);

        source.set(5);
        assert!(computed.is_dirty());
        assert_eq!(computed.get(), 10);
        assert_eq!(computed.version(), 2);
    }

    #[test]
    fn multi_dep_computed() {
        let width = Observable::new(10);
        let height = Observable::new(20);
        let area = Computed::from2(&width, &height, |w, h| w * h);

        assert_eq!(area.get(), 200);

        width.set(5);
        assert_eq!(area.get(), 100);

        height.set(30);
        assert_eq!(area.get(), 150);
    }

    #[test]
    fn three_dep_computed() {
        let a = Observable::new(1);
        let b = Observable::new(2);
        let c = Observable::new(3);
        let sum = Computed::from3(&a, &b, &c, |x, y, z| x + y + z);

        assert_eq!(sum.get(), 6);

        a.set(10);
        assert_eq!(sum.get(), 15);

        c.set(100);
        assert_eq!(sum.get(), 112);
    }

    #[test]
    fn lazy_evaluation() {
        let compute_count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&compute_count);

        let source = Observable::new(42);
        let source_clone = source.clone();
        let computed = Computed::from_fn(
            move || {
                count_clone.set(count_clone.get() + 1);
                source_clone.get() * 2
            },
            vec![],
        );

        // Not computed yet.
        assert_eq!(compute_count.get(), 0);

        // First get triggers computation.
        assert_eq!(computed.get(), 84);
        assert_eq!(compute_count.get(), 1);

        // Second get returns cached (not dirty without subscription).
        assert_eq!(computed.get(), 84);
        assert_eq!(compute_count.get(), 1);
    }

    #[test]
    fn memoization() {
        let compute_count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&compute_count);

        let source = Observable::new(10);
        let computed = Computed::from_observable(&source, move |v| {
            count_clone.set(count_clone.get() + 1);
            v * 2
        });

        // First get.
        assert_eq!(computed.get(), 20);
        assert_eq!(compute_count.get(), 1);

        // Cached — no recompute.
        assert_eq!(computed.get(), 20);
        assert_eq!(compute_count.get(), 1);

        // Change source — recompute on next get.
        source.set(20);
        assert_eq!(computed.get(), 40);
        assert_eq!(compute_count.get(), 2);

        // Cached again.
        assert_eq!(computed.get(), 40);
        assert_eq!(compute_count.get(), 2);
    }

    #[test]
    fn invalidate_forces_recompute() {
        let compute_count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&compute_count);

        let source = Observable::new(5);
        let computed = Computed::from_observable(&source, move |v| {
            count_clone.set(count_clone.get() + 1);
            *v
        });

        assert_eq!(computed.get(), 5);
        assert_eq!(compute_count.get(), 1);

        computed.invalidate();
        assert!(computed.is_dirty());

        assert_eq!(computed.get(), 5);
        assert_eq!(compute_count.get(), 2);
    }

    #[test]
    fn with_access() {
        let source = Observable::new(vec![1, 2, 3]);
        let computed = Computed::from_observable(&source, |v| v.iter().sum::<i32>());

        let result = computed.with(|sum| *sum);
        assert_eq!(result, 6);
    }

    #[test]
    fn version_increments_on_recompute() {
        let source = Observable::new(0);
        let computed = Computed::from_observable(&source, |v| *v);

        assert_eq!(computed.version(), 0);

        // First get triggers compute.
        let _ = computed.get();
        assert_eq!(computed.version(), 1);

        // Same source, no change — no recompute.
        let _ = computed.get();
        assert_eq!(computed.version(), 1);

        // Source changes.
        source.set(1);
        let _ = computed.get();
        assert_eq!(computed.version(), 2);
    }

    #[test]
    fn clone_shares_state() {
        let source = Observable::new(10);
        let c1 = Computed::from_observable(&source, |v| v + 1);
        let c2 = c1.clone();

        assert_eq!(c1.get(), 11);
        assert_eq!(c2.get(), 11);

        source.set(20);
        assert_eq!(c1.get(), 21);
        // c2 shares state, so it also sees the new value.
        assert_eq!(c2.get(), 21);
    }

    #[test]
    fn diamond_dependency() {
        // A -> B, A -> C, (B, C) -> D
        let a = Observable::new(10);

        let b = Computed::from_observable(&a, |v| v + 1);
        let c = Computed::from_observable(&a, |v| v * 2);

        // D depends on B and C indirectly (through their computed values).
        let b_clone = b.clone();
        let c_clone = c.clone();
        let d = Computed::from_observable(&a, move |_| b_clone.get() + c_clone.get());

        assert_eq!(b.get(), 11);
        assert_eq!(c.get(), 20);
        assert_eq!(d.get(), 31);

        a.set(5);
        assert_eq!(b.get(), 6);
        assert_eq!(c.get(), 10);
        assert_eq!(d.get(), 16);
    }

    #[test]
    fn no_change_same_value() {
        let source = Observable::new(42);
        let compute_count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&compute_count);

        let computed = Computed::from_observable(&source, move |v| {
            count_clone.set(count_clone.get() + 1);
            *v
        });

        let _ = computed.get();
        assert_eq!(compute_count.get(), 1);

        // Set same value — Observable won't notify, so computed stays clean.
        source.set(42);
        assert!(!computed.is_dirty());
        let _ = computed.get();
        assert_eq!(compute_count.get(), 1);
    }

    #[test]
    fn debug_format() {
        let source = Observable::new(42);
        let computed = Computed::from_observable(&source, |v| *v);
        let _ = computed.get();
        let dbg = format!("{:?}", computed);
        assert!(dbg.contains("Computed"));
        assert!(dbg.contains("42"));
    }

    #[test]
    fn from_fn_with_manual_subscriptions() {
        let source = Observable::new(10);

        // Use from_observable for proper dirty wiring.
        let computed = Computed::from_observable(&source, |v| v * 3);

        assert_eq!(computed.get(), 30);

        source.set(20);
        assert_eq!(computed.get(), 60);

        // Also test from_fn: it doesn't auto-wire dirty, so the caller
        // must call invalidate() or rely on subscriptions that do so.
        let source2 = Observable::new(5);
        let s2_clone = source2.clone();

        let inner_dirty = Rc::new(Cell::new(false));
        let dirty_for_sub = Rc::clone(&inner_dirty);

        // This subscription tracks changes but can't reach the computed's
        // dirty flag. We just verify from_fn keeps subscriptions alive.
        let sub = source2.subscribe(move |_| {
            dirty_for_sub.set(true);
        });

        let computed2 = Computed::from_fn(move || s2_clone.get() * 3, vec![sub]);

        assert_eq!(computed2.get(), 15);

        source2.set(10);
        assert!(inner_dirty.get()); // Our external flag was set.
        // from_fn doesn't auto-dirty, so we must invalidate manually.
        computed2.invalidate();
        assert_eq!(computed2.get(), 30);
    }

    #[test]
    fn string_computed() {
        let first = Observable::new("John".to_string());
        let last = Observable::new("Doe".to_string());
        let full_name = Computed::from2(&first, &last, |f, l| format!("{} {}", f, l));

        assert_eq!(full_name.get(), "John Doe");

        first.set("Jane".to_string());
        assert_eq!(full_name.get(), "Jane Doe");

        last.set("Smith".to_string());
        assert_eq!(full_name.get(), "Jane Smith");
    }

    #[test]
    fn computed_survives_source_drop() {
        let computed;
        {
            let source = Observable::new(42);
            computed = Computed::from_observable(&source, |v| *v);
            let _ = computed.get(); // Cache the value.
        }
        // Source dropped, but computed retains its last cached value.
        assert_eq!(computed.get(), 42);
        assert!(!computed.is_dirty());
    }

    #[test]
    fn is_dirty_initially_true() {
        let source = Observable::new(1);
        let computed = Computed::from_observable(&source, |v| *v);
        assert!(computed.is_dirty());
    }

    #[test]
    fn with_increments_version_on_dirty() {
        let source = Observable::new(10);
        let computed = Computed::from_observable(&source, |v| *v);

        // First access via with()
        let val = computed.with(|v| *v);
        assert_eq!(val, 10);
        assert_eq!(computed.version(), 1);

        // Change source and access via with()
        source.set(20);
        let val = computed.with(|v| *v);
        assert_eq!(val, 20);
        assert_eq!(computed.version(), 2);
    }

    #[test]
    fn invalidate_without_source_change() {
        let source = Observable::new(5);
        let computed = Computed::from_observable(&source, |v| *v);

        let _ = computed.get();
        assert_eq!(computed.version(), 1);
        assert!(!computed.is_dirty());

        computed.invalidate();
        assert!(computed.is_dirty());

        // get() triggers recomputation even though source didn't change
        let _ = computed.get();
        assert_eq!(computed.version(), 2);
    }

    #[test]
    fn many_updates_version_monotonic() {
        let source = Observable::new(0);
        let computed = Computed::from_observable(&source, |v| *v);

        for i in 1..=50 {
            source.set(i);
            let _ = computed.get();
        }
        // 50 updates, each triggering a recomputation.
        assert_eq!(computed.version(), 50);
    }
}
