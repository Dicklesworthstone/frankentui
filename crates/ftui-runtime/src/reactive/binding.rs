#![forbid(unsafe_code)]

//! Ergonomic binding utilities for connecting [`Observable`] values to UI state.
//!
//! A [`Binding<T>`] encapsulates an observable source plus an optional transform,
//! making it easy to derive display values from reactive state. The [`bind!`] and
//! [`bind_map!`] macros provide syntactic sugar.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::reactive::{Observable, Binding, bind, bind_map};
//!
//! let count = Observable::new(0);
//!
//! // Direct binding — get() returns the observable's value.
//! let b = bind!(count);
//! assert_eq!(b.get(), 0);
//!
//! // Mapped binding — get() returns the transformed value.
//! let label = bind_map!(count, |c| format!("Count: {c}"));
//! assert_eq!(label.get(), "Count: 0");
//!
//! count.set(5);
//! assert_eq!(b.get(), 5);
//! assert_eq!(label.get(), "Count: 5");
//! ```
//!
//! # Two-Way Bindings
//!
//! [`TwoWayBinding<T>`] connects two `Observable`s so changes to either
//! propagate to the other, with cycle prevention.
//!
//! ```ignore
//! let source = Observable::new(42);
//! let target = Observable::new(0);
//! let _binding = TwoWayBinding::new(&source, &target);
//!
//! source.set(10);
//! assert_eq!(target.get(), 10);
//!
//! target.set(20);
//! assert_eq!(source.get(), 20);
//! ```
//!
//! # Invariants
//!
//! 1. `Binding::get()` always returns the current (not stale) value.
//! 2. A binding's transform is applied on every `get()` call (no caching).
//!    Use [`Computed`] when memoization is needed.
//! 3. `TwoWayBinding` prevents infinite cycles via a re-entrancy guard.
//! 4. Dropping a `TwoWayBinding` cleanly unsubscribes both directions.
//! 5. Bindings are `Clone` when the source `Observable` is (shared state).
//!
//! # Failure Modes
//!
//! - Transform panic: propagates to caller of `get()`.
//! - Source dropped while binding alive: binding still works (Rc keeps inner alive).
//!
//! [`Computed`]: super::Computed

use std::cell::Cell;
use std::rc::Rc;

use super::observable::{Observable, Subscription};

// ---------------------------------------------------------------------------
// Binding<T> — one-way read binding
// ---------------------------------------------------------------------------

/// A read-only binding to an [`Observable`] value with an optional transform.
///
/// Evaluates lazily on each `get()` call. For memoized transforms, prefer
/// [`Computed`](super::Computed).
pub struct Binding<T> {
    eval: Rc<dyn Fn() -> T>,
}

impl<T> Clone for Binding<T> {
    fn clone(&self) -> Self {
        Self {
            eval: Rc::clone(&self.eval),
        }
    }
}

impl<T: std::fmt::Debug + 'static> std::fmt::Debug for Binding<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("value", &self.get())
            .finish()
    }
}

impl<T: 'static> Binding<T> {
    /// Create a binding that evaluates `f` on each `get()` call.
    pub fn new(f: impl Fn() -> T + 'static) -> Self {
        Self { eval: Rc::new(f) }
    }

    /// Get the current bound value.
    #[must_use]
    pub fn get(&self) -> T {
        (self.eval)()
    }

    /// Apply a further transform, returning a new `Binding`.
    pub fn then<U: 'static>(self, f: impl Fn(T) -> U + 'static) -> Binding<U> {
        Binding {
            eval: Rc::new(move || f((self.eval)())),
        }
    }
}

/// Create a direct binding to an observable (identity transform).
pub fn bind_observable<T: Clone + PartialEq + 'static>(source: &Observable<T>) -> Binding<T> {
    let src = source.clone();
    Binding {
        eval: Rc::new(move || src.get()),
    }
}

/// Create a mapped binding: `source` value transformed by `map`.
pub fn bind_mapped<S: Clone + PartialEq + 'static, T: 'static>(
    source: &Observable<S>,
    map: impl Fn(&S) -> T + 'static,
) -> Binding<T> {
    let src = source.clone();
    Binding {
        eval: Rc::new(move || src.with(|v| map(v))),
    }
}

/// Create a binding from two observables combined by `map`.
pub fn bind_mapped2<S1: Clone + PartialEq + 'static, S2: Clone + PartialEq + 'static, T: 'static>(
    s1: &Observable<S1>,
    s2: &Observable<S2>,
    map: impl Fn(&S1, &S2) -> T + 'static,
) -> Binding<T> {
    let src1 = s1.clone();
    let src2 = s2.clone();
    Binding {
        eval: Rc::new(move || {
            src1.with(|v1| src2.with(|v2| map(v1, v2)))
        }),
    }
}

// ---------------------------------------------------------------------------
// TwoWayBinding<T> — bidirectional sync
// ---------------------------------------------------------------------------

/// Bidirectional binding between two [`Observable`]s of the same type.
///
/// Changes to either observable propagate to the other. A re-entrancy guard
/// prevents infinite update cycles.
///
/// Drop the `TwoWayBinding` to disconnect both directions.
pub struct TwoWayBinding<T: Clone + PartialEq + 'static> {
    _sub_a_to_b: Subscription,
    _sub_b_to_a: Subscription,
    _guard: Rc<Cell<bool>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Clone + PartialEq + 'static> TwoWayBinding<T> {
    /// Create a two-way binding between `a` and `b`.
    ///
    /// Initially syncs `b` to `a`'s current value. Subsequent changes to
    /// either side propagate to the other.
    pub fn new(a: &Observable<T>, b: &Observable<T>) -> Self {
        // Sync initial value: b takes a's value.
        b.set(a.get());

        let syncing = Rc::new(Cell::new(false));

        // a → b
        let b_clone = b.clone();
        let guard_ab = Rc::clone(&syncing);
        let sub_ab = a.subscribe(move |val| {
            if !guard_ab.get() {
                guard_ab.set(true);
                b_clone.set(val.clone());
                guard_ab.set(false);
            }
        });

        // b → a
        let a_clone = a.clone();
        let guard_ba = Rc::clone(&syncing);
        let sub_ba = b.subscribe(move |val| {
            if !guard_ba.get() {
                guard_ba.set(true);
                a_clone.set(val.clone());
                guard_ba.set(false);
            }
        });

        Self {
            _sub_a_to_b: sub_ab,
            _sub_b_to_a: sub_ba,
            _guard: syncing,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T: Clone + PartialEq + 'static> std::fmt::Debug for TwoWayBinding<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwoWayBinding").finish()
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// Create a direct [`Binding`] to an observable.
///
/// # Examples
///
/// ```ignore
/// let count = Observable::new(0);
/// let b = bind!(count);
/// assert_eq!(b.get(), 0);
/// ```
#[macro_export]
macro_rules! bind {
    ($obs:expr) => {
        $crate::reactive::binding::bind_observable(&$obs)
    };
}

/// Create a mapped [`Binding`] from an observable with a transform function.
///
/// # Examples
///
/// ```ignore
/// let count = Observable::new(0);
/// let label = bind_map!(count, |c| format!("Count: {c}"));
/// assert_eq!(label.get(), "Count: 0");
/// ```
#[macro_export]
macro_rules! bind_map {
    ($obs:expr, $f:expr) => {
        $crate::reactive::binding::bind_mapped(&$obs, $f)
    };
}

/// Create a mapped [`Binding`] from two observables.
///
/// # Examples
///
/// ```ignore
/// let width = Observable::new(10);
/// let height = Observable::new(20);
/// let area = bind_map2!(width, height, |w, h| w * h);
/// assert_eq!(area.get(), 200);
/// ```
#[macro_export]
macro_rules! bind_map2 {
    ($s1:expr, $s2:expr, $f:expr) => {
        $crate::reactive::binding::bind_mapped2(&$s1, &$s2, $f)
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_from_observable() {
        let obs = Observable::new(42);
        let b = bind_observable(&obs);
        assert_eq!(b.get(), 42);

        obs.set(100);
        assert_eq!(b.get(), 100);
    }

    #[test]
    fn binding_map() {
        let count = Observable::new(3);
        let label = bind_mapped(&count, |c| format!("items: {c}"));
        assert_eq!(label.get(), "items: 3");

        count.set(7);
        assert_eq!(label.get(), "items: 7");
    }

    #[test]
    fn binding_map2() {
        let w = Observable::new(10);
        let h = Observable::new(20);
        let area = bind_mapped2(&w, &h, |a, b| a * b);
        assert_eq!(area.get(), 200);

        w.set(5);
        assert_eq!(area.get(), 100);
    }

    #[test]
    fn binding_then_chain() {
        let obs = Observable::new(5);
        let doubled = bind_observable(&obs).then(|v| v * 2);
        assert_eq!(doubled.get(), 10);

        obs.set(3);
        assert_eq!(doubled.get(), 6);
    }

    #[test]
    fn binding_clone_shares_source() {
        let obs = Observable::new(1);
        let b1 = bind_observable(&obs);
        let b2 = b1.clone();

        obs.set(99);
        assert_eq!(b1.get(), 99);
        assert_eq!(b2.get(), 99);
    }

    #[test]
    fn binding_new_custom() {
        let counter = Rc::new(Cell::new(0));
        let c = Rc::clone(&counter);
        let b = Binding::new(move || {
            c.set(c.get() + 1);
            c.get()
        });
        assert_eq!(b.get(), 1);
        assert_eq!(b.get(), 2);
    }

    #[test]
    fn bind_macro() {
        let obs = Observable::new(42);
        let b = bind!(obs);
        assert_eq!(b.get(), 42);
    }

    #[test]
    fn bind_map_macro() {
        let obs = Observable::new(5);
        let b = bind_map!(obs, |v| v * 10);
        assert_eq!(b.get(), 50);
    }

    #[test]
    fn bind_map2_macro() {
        let a = Observable::new(3);
        let b = Observable::new(4);
        let sum = bind_map2!(a, b, |x, y| x + y);
        assert_eq!(sum.get(), 7);
    }

    // ---- Two-way binding tests ----

    #[test]
    fn two_way_initial_sync() {
        let a = Observable::new(10);
        let b = Observable::new(0);
        let _binding = TwoWayBinding::new(&a, &b);
        assert_eq!(b.get(), 10, "b should sync to a's initial value");
    }

    #[test]
    fn two_way_a_to_b() {
        let a = Observable::new(1);
        let b = Observable::new(0);
        let _binding = TwoWayBinding::new(&a, &b);

        a.set(42);
        assert_eq!(b.get(), 42);
    }

    #[test]
    fn two_way_b_to_a() {
        let a = Observable::new(1);
        let b = Observable::new(0);
        let _binding = TwoWayBinding::new(&a, &b);

        b.set(99);
        assert_eq!(a.get(), 99);
    }

    #[test]
    fn two_way_no_cycle() {
        let a = Observable::new(0);
        let b = Observable::new(0);
        let _binding = TwoWayBinding::new(&a, &b);

        // Set a → should propagate to b but not cycle back.
        a.set(5);
        assert_eq!(a.get(), 5);
        assert_eq!(b.get(), 5);

        b.set(10);
        assert_eq!(a.get(), 10);
        assert_eq!(b.get(), 10);
    }

    #[test]
    fn two_way_drop_disconnects() {
        let a = Observable::new(1);
        let b = Observable::new(0);
        {
            let _binding = TwoWayBinding::new(&a, &b);
            a.set(5);
            assert_eq!(b.get(), 5);
        }
        // After drop, changes should not propagate.
        a.set(100);
        assert_eq!(b.get(), 5, "b should not update after binding dropped");
    }

    #[test]
    fn two_way_with_strings() {
        let a = Observable::new(String::from("hello"));
        let b = Observable::new(String::new());
        let _binding = TwoWayBinding::new(&a, &b);

        assert_eq!(b.get(), "hello");
        b.set("world".to_string());
        assert_eq!(a.get(), "world");
    }

    #[test]
    fn multiple_bindings_same_source() {
        let source = Observable::new(0);
        let b1 = bind_observable(&source);
        let b2 = bind_mapped(&source, |v| v * 2);
        let b3 = bind_mapped(&source, |v| format!("{v}"));

        source.set(5);
        assert_eq!(b1.get(), 5);
        assert_eq!(b2.get(), 10);
        assert_eq!(b3.get(), "5");
    }

    #[test]
    fn binding_survives_source_clone() {
        let source = Observable::new(42);
        let b = bind_observable(&source);

        let source2 = source.clone();
        source2.set(99);
        assert_eq!(b.get(), 99, "binding should see changes through cloned observable");
    }
}
