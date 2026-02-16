#![forbid(unsafe_code)]

//! Observable value wrapper with change notification and version tracking.
//!
//! # Design
//!
//! [`Observable<T>`] wraps a value of type `T` in shared, reference-counted
//! storage (`Rc<RefCell<..>>`). When the value changes (determined by
//! `PartialEq`), all live subscribers are notified in registration order.
//!
//! # Performance
//!
//! | Operation    | Complexity               |
//! |-------------|--------------------------|
//! | `get()`     | O(1)                     |
//! | `set()`     | O(S) where S = subscribers |
//! | `subscribe()` | O(1) amortized          |
//! | Memory      | ~48 bytes + sizeof(T)    |
//!
//! # Failure Modes
//!
//! - **Re-entrant set**: Calling `set()` from within a subscriber callback
//!   will panic (RefCell borrow rules). This is intentional: re-entrant
//!   mutations indicate a design bug in the subscriber graph.
//! - **Subscriber leak**: If `Subscription` guards are stored indefinitely
//!   without being dropped, callbacks accumulate. Dead weak references are
//!   cleaned lazily during `notify()`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use tracing::{info, info_span};
use web_time::Instant;

/// A subscriber callback stored as a strong `Rc` internally, handed out
/// as `Weak` to the observable.
type CallbackRc<T> = Rc<dyn Fn(&T)>;
type CallbackWeak<T> = Weak<dyn Fn(&T)>;

/// Shared interior for [`Observable<T>`].
struct ObservableInner<T> {
    value: T,
    version: u64,
    /// Subscribers stored as weak references. Dead entries are pruned on notify.
    subscribers: Vec<CallbackWeak<T>>,
}

/// A shared, version-tracked value with change notification.
///
/// Cloning an `Observable` creates a new handle to the **same** inner state —
/// both handles see the same value and share subscribers.
///
/// # Invariants
///
/// 1. `version` increments by exactly 1 on each value-changing mutation.
/// 2. `set(v)` where `v == current` is a no-op.
/// 3. Subscribers are notified in registration order.
/// 4. Dead subscribers (dropped [`Subscription`] guards) are pruned lazily.
pub struct Observable<T> {
    inner: Rc<RefCell<ObservableInner<T>>>,
}

// Manual Clone: shares the same Rc.
impl<T> Clone for Observable<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Observable<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.borrow();
        f.debug_struct("Observable")
            .field("value", &inner.value)
            .field("version", &inner.version)
            .field("subscriber_count", &inner.subscribers.len())
            .finish()
    }
}

impl<T: Clone + PartialEq + 'static> Observable<T> {
    /// Create a new observable with the given initial value.
    ///
    /// The initial version is 0 and no subscribers are registered.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            inner: Rc::new(RefCell::new(ObservableInner {
                value,
                version: 0,
                subscribers: Vec::new(),
            })),
        }
    }

    /// Get a clone of the current value.
    #[must_use]
    pub fn get(&self) -> T {
        self.inner.borrow().value.clone()
    }

    /// Access the current value by reference without cloning.
    ///
    /// The closure `f` receives an immutable reference to the value.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.inner.borrow().value)
    }

    /// Set a new value. If the new value differs from the current value
    /// (by `PartialEq`), the version is incremented and all live subscribers
    /// are notified.
    ///
    /// This method is safe to call re-entrantly from within subscriber callbacks.
    pub fn set(&self, value: T) {
        let changed = {
            let mut inner = self.inner.borrow_mut();
            if inner.value == value {
                return;
            }
            inner.value = value;
            inner.version += 1;
            true
        };
        if changed {
            self.notify();
        }
    }

    /// Modify the value in place via a closure. If the value changes
    /// (compared by `PartialEq` against a snapshot), the version is
    /// incremented and subscribers are notified.
    ///
    /// This method is safe to call re-entrantly from within subscriber callbacks.
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        let changed = {
            let mut inner = self.inner.borrow_mut();
            let old = inner.value.clone();
            f(&mut inner.value);
            if inner.value != old {
                inner.version += 1;
                true
            } else {
                false
            }
        };
        if changed {
            self.notify();
        }
    }

    /// Subscribe to value changes. The callback is invoked with a reference
    /// to the new value each time it changes.
    ///
    /// Returns a [`Subscription`] guard. Dropping the guard unsubscribes
    /// the callback (it will not be called after drop, though it may still
    /// be in the subscriber list until the next `notify()` prunes it).
    pub fn subscribe(&self, callback: impl Fn(&T) + 'static) -> Subscription {
        let strong: CallbackRc<T> = Rc::new(callback);
        let weak = Rc::downgrade(&strong);
        self.inner.borrow_mut().subscribers.push(weak);
        // Wrap in a holder struct that can be type-erased as `dyn Any`,
        // since `Rc<dyn Fn(&T)>` itself cannot directly coerce to `Rc<dyn Any>`.
        Subscription {
            _guard: Box::new(strong),
        }
    }

    /// Current version number. Increments by 1 on each value-changing
    /// mutation. Useful for dirty-checking in render loops.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.borrow().version
    }

    /// Number of currently registered subscribers (including dead ones
    /// not yet pruned).
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.inner.borrow().subscribers.len()
    }

    /// Notify live subscribers and prune dead ones.
    ///
    /// If a batch scope is active (see [`super::batch::BatchScope`]),
    /// notifications are deferred until the batch exits.
    fn notify(&self) {
        // Collect live callbacks first (to avoid holding the borrow during calls).
        let callbacks: Vec<CallbackRc<T>> = {
            let mut inner = self.inner.borrow_mut();
            // Prune dead weak refs and collect live ones.
            inner.subscribers.retain(|w| w.strong_count() > 0);
            inner
                .subscribers
                .iter()
                .filter_map(|w| w.upgrade())
                .collect()
        };

        if callbacks.is_empty() {
            return;
        }

        let widgets_invalidated = callbacks.len() as u64;

        if super::batch::is_batching() {
            super::batch::record_rows_changed(1);
            // Defer each callback to the batch queue.
            for cb in callbacks {
                let callback_key = Rc::as_ptr(&cb) as *const () as usize;
                let source = self.clone();
                super::batch::defer_or_run_keyed(callback_key, move || {
                    let latest = source.get();
                    cb(&latest);
                });
            }
            return;
        }

        // Clone the value once for all callbacks.
        let value = self.inner.borrow().value.clone();
        let propagation_start = Instant::now();
        let _span = info_span!(
            "bloodstream.delta",
            rows_changed = 1_u64,
            widgets_invalidated,
            duration_us = tracing::field::Empty
        )
        .entered();

        // Fire immediately.
        for cb in &callbacks {
            cb(&value);
        }

        let duration_us = propagation_start.elapsed().as_micros() as u64;
        tracing::Span::current().record("duration_us", duration_us);
        info!(
            bloodstream_propagation_duration_us = duration_us,
            rows_changed = 1_u64,
            widgets_invalidated,
            "bloodstream propagation duration histogram"
        );
    }
}

/// RAII guard for a subscriber callback.
///
/// Dropping the `Subscription` causes the associated callback to become
/// unreachable (the strong `Rc` is dropped, so the `Weak` in the
/// observable's subscriber list will fail to upgrade on the next
/// notification cycle).
pub struct Subscription {
    /// Type-erased strong reference keeping the callback `Rc` alive.
    /// When this `Box<dyn Any>` is dropped, the inner `Rc<dyn Fn(&T)>`
    /// is dropped, and the corresponding `Weak` in the subscriber list
    /// loses its referent.
    _guard: Box<dyn std::any::Any>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TableSnapshot {
        schema_version: u64,
        rows: Vec<String>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RenderMode {
        PartialDelta,
        FullRerender,
    }

    fn classify_render_mode(previous: &TableSnapshot, next: &TableSnapshot) -> RenderMode {
        if previous.schema_version != next.schema_version {
            RenderMode::FullRerender
        } else {
            RenderMode::PartialDelta
        }
    }

    #[derive(Default)]
    struct DeltaSpanVisitor {
        rows_changed: Option<u64>,
        widgets_invalidated: Option<u64>,
    }

    impl Visit for DeltaSpanVisitor {
        fn record_u64(&mut self, field: &Field, value: u64) {
            match field.name() {
                "rows_changed" => self.rows_changed = Some(value),
                "widgets_invalidated" => self.widgets_invalidated = Some(value),
                _ => {}
            }
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            if value < 0 {
                return;
            }
            self.record_u64(field, value as u64);
        }

        fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
    }

    struct DeltaSpanSubscriber {
        next_id: AtomicU64,
        spans: Arc<Mutex<Vec<(u64, u64)>>>,
    }

    impl tracing::Subscriber for DeltaSpanSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            if attrs.metadata().name() == "bloodstream.delta" {
                let mut visitor = DeltaSpanVisitor::default();
                attrs.record(&mut visitor);
                self.spans.lock().expect("span capture lock").push((
                    visitor.rows_changed.unwrap_or(0),
                    visitor.widgets_invalidated.unwrap_or(0),
                ));
            }
            tracing::span::Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed))
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn capture_delta_spans(run: impl FnOnce()) -> Vec<(u64, u64)> {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let subscriber = DeltaSpanSubscriber {
            next_id: AtomicU64::new(1),
            spans: Arc::clone(&spans),
        };
        let _guard = tracing::subscriber::set_default(subscriber);
        run();
        spans.lock().expect("span capture lock").clone()
    }

    #[test]
    fn get_set_basic() {
        let obs = Observable::new(42);
        assert_eq!(obs.get(), 42);
        assert_eq!(obs.version(), 0);

        obs.set(99);
        assert_eq!(obs.get(), 99);
        assert_eq!(obs.version(), 1);
    }

    #[test]
    fn no_change_no_version_bump() {
        let obs = Observable::new(42);
        obs.set(42); // Same value.
        assert_eq!(obs.version(), 0);
    }

    #[test]
    fn with_access() {
        let obs = Observable::new(vec![1, 2, 3]);
        let sum = obs.with(|v| v.iter().sum::<i32>());
        assert_eq!(sum, 6);
    }

    #[test]
    fn update_mutates_in_place() {
        let obs = Observable::new(vec![1, 2, 3]);
        obs.update(|v| v.push(4));
        assert_eq!(obs.get(), vec![1, 2, 3, 4]);
        assert_eq!(obs.version(), 1);
    }

    #[test]
    fn update_no_change_no_bump() {
        let obs = Observable::new(10);
        obs.update(|v| {
            *v = 10; // Same value.
        });
        assert_eq!(obs.version(), 0);
    }

    #[test]
    fn change_notification() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let _sub = obs.subscribe(move |_val| {
            count_clone.set(count_clone.get() + 1);
        });

        obs.set(1);
        assert_eq!(count.get(), 1);

        obs.set(2);
        assert_eq!(count.get(), 2);

        // Same value — no notification.
        obs.set(2);
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn subscriber_receives_new_value() {
        let obs = Observable::new(0);
        let last_seen = Rc::new(Cell::new(0));
        let last_clone = Rc::clone(&last_seen);

        let _sub = obs.subscribe(move |val| {
            last_clone.set(*val);
        });

        obs.set(42);
        assert_eq!(last_seen.get(), 42);

        obs.set(99);
        assert_eq!(last_seen.get(), 99);
    }

    #[test]
    fn subscription_drop_unsubscribes() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let sub = obs.subscribe(move |_val| {
            count_clone.set(count_clone.get() + 1);
        });

        obs.set(1);
        assert_eq!(count.get(), 1);

        drop(sub);

        obs.set(2);
        // Callback should NOT have been called.
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn multiple_subscribers() {
        let obs = Observable::new(0);
        let a = Rc::new(Cell::new(0u32));
        let b = Rc::new(Cell::new(0u32));
        let a_clone = Rc::clone(&a);
        let b_clone = Rc::clone(&b);

        let _sub_a = obs.subscribe(move |_| a_clone.set(a_clone.get() + 1));
        let _sub_b = obs.subscribe(move |_| b_clone.set(b_clone.get() + 1));

        obs.set(1);
        assert_eq!(a.get(), 1);
        assert_eq!(b.get(), 1);

        obs.set(2);
        assert_eq!(a.get(), 2);
        assert_eq!(b.get(), 2);
    }

    #[test]
    fn version_increment() {
        let obs = Observable::new("hello".to_string());
        assert_eq!(obs.version(), 0);

        obs.set("world".to_string());
        assert_eq!(obs.version(), 1);

        obs.set("!".to_string());
        assert_eq!(obs.version(), 2);

        // Same value, no increment.
        obs.set("!".to_string());
        assert_eq!(obs.version(), 2);
    }

    #[test]
    fn clone_shares_state() {
        let obs1 = Observable::new(0);
        let obs2 = obs1.clone();

        obs1.set(42);
        assert_eq!(obs2.get(), 42);
        assert_eq!(obs2.version(), 1);

        obs2.set(99);
        assert_eq!(obs1.get(), 99);
        assert_eq!(obs1.version(), 2);
    }

    #[test]
    fn clone_shares_subscribers() {
        let obs1 = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let _sub = obs1.subscribe(move |_| count_clone.set(count_clone.get() + 1));

        let obs2 = obs1.clone();
        obs2.set(1);
        assert_eq!(count.get(), 1); // Subscriber sees change via clone.
    }

    #[test]
    fn subscriber_count() {
        let obs = Observable::new(0);
        assert_eq!(obs.subscriber_count(), 0);

        let _s1 = obs.subscribe(|_| {});
        assert_eq!(obs.subscriber_count(), 1);

        let s2 = obs.subscribe(|_| {});
        assert_eq!(obs.subscriber_count(), 2);

        drop(s2);
        // Dead subscriber not yet pruned.
        assert_eq!(obs.subscriber_count(), 2);

        // Trigger notify to prune dead.
        obs.set(1);
        assert_eq!(obs.subscriber_count(), 1);
    }

    #[test]
    fn debug_format() {
        let obs = Observable::new(42);
        let dbg = format!("{:?}", obs);
        assert!(dbg.contains("Observable"));
        assert!(dbg.contains("42"));
        assert!(dbg.contains("version"));
    }

    #[test]
    fn notification_order_is_registration_order() {
        let obs = Observable::new(0);
        let log = Rc::new(RefCell::new(Vec::new()));

        let log1 = Rc::clone(&log);
        let _s1 = obs.subscribe(move |_| log1.borrow_mut().push('A'));

        let log2 = Rc::clone(&log);
        let _s2 = obs.subscribe(move |_| log2.borrow_mut().push('B'));

        let log3 = Rc::clone(&log);
        let _s3 = obs.subscribe(move |_| log3.borrow_mut().push('C'));

        obs.set(1);
        assert_eq!(*log.borrow(), vec!['A', 'B', 'C']);
    }

    #[test]
    fn update_with_subscriber() {
        let obs = Observable::new(vec![1, 2, 3]);
        let last_len = Rc::new(Cell::new(0usize));
        let last_clone = Rc::clone(&last_len);

        let _sub = obs.subscribe(move |v: &Vec<i32>| {
            last_clone.set(v.len());
        });

        obs.update(|v| v.push(4));
        assert_eq!(last_len.get(), 4);
    }

    #[test]
    fn many_set_calls_version_monotonic() {
        let obs = Observable::new(0);
        for i in 1..=100 {
            obs.set(i);
        }
        assert_eq!(obs.version(), 100);
        assert_eq!(obs.get(), 100);
    }

    #[test]
    fn partial_subscriber_drop() {
        let obs = Observable::new(0);
        let a = Rc::new(Cell::new(0u32));
        let b = Rc::new(Cell::new(0u32));
        let a_clone = Rc::clone(&a);
        let b_clone = Rc::clone(&b);

        let sub_a = obs.subscribe(move |_| a_clone.set(a_clone.get() + 1));
        let _sub_b = obs.subscribe(move |_| b_clone.set(b_clone.get() + 1));

        obs.set(1);
        assert_eq!(a.get(), 1);
        assert_eq!(b.get(), 1);

        drop(sub_a);

        obs.set(2);
        assert_eq!(a.get(), 1); // A was unsubscribed.
        assert_eq!(b.get(), 2); // B still active.
    }

    #[test]
    fn single_row_change_propagates_only_to_bound_widgets() {
        let row_a = Observable::new(vec!["a".to_string()]);
        let row_b = Observable::new(vec!["b".to_string()]);
        let a_hits = Rc::new(Cell::new(0u32));
        let b_hits = Rc::new(Cell::new(0u32));
        let a_hits_clone = Rc::clone(&a_hits);
        let b_hits_clone = Rc::clone(&b_hits);

        let _sub_a = row_a.subscribe(move |_| a_hits_clone.set(a_hits_clone.get() + 1));
        let _sub_b = row_b.subscribe(move |_| b_hits_clone.set(b_hits_clone.get() + 1));

        row_a.set(vec!["a2".to_string()]);
        assert_eq!(a_hits.get(), 1, "bound row-A widget should be invalidated");
        assert_eq!(
            b_hits.get(),
            0,
            "unbound row-B widget should remain untouched"
        );
    }

    #[test]
    fn batch_delta_propagates_atomically_without_stale_intermediate_values() {
        let rows = Observable::new(vec!["r0".to_string()]);
        let seen = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
        let seen_clone = Rc::clone(&seen);
        let _sub = rows.subscribe(move |current| seen_clone.borrow_mut().push(current.clone()));

        {
            let _batch = crate::reactive::batch::BatchScope::new();
            rows.set(vec!["r1".to_string()]);
            rows.set(vec!["r1".to_string(), "r2".to_string()]);
            rows.update(|current| current.push("r3".to_string()));
            assert!(
                seen.borrow().is_empty(),
                "callbacks must be deferred until batch exit"
            );
        }

        let snapshots = seen.borrow();
        assert_eq!(
            snapshots.len(),
            1,
            "batched updates should coalesce to one invalidation"
        );
        assert_eq!(
            snapshots[0],
            vec!["r1".to_string(), "r2".to_string(), "r3".to_string()],
            "subscriber must observe only final state"
        );
    }

    #[test]
    fn unbound_table_updates_produce_no_bloodstream_delta() {
        let table_rows = Observable::new(vec!["old".to_string()]);
        let spans = capture_delta_spans(|| {
            table_rows.set(vec!["new".to_string()]);
        });
        assert!(
            spans.is_empty(),
            "unbound table updates should not emit bloodstream deltas"
        );
    }

    #[test]
    fn bloodstream_delta_span_reports_rows_changed_and_widgets_invalidated() {
        let table_rows = Observable::new(vec!["old".to_string()]);
        let _sub_a = table_rows.subscribe(|_| {});
        let _sub_b = table_rows.subscribe(|_| {});

        let spans = capture_delta_spans(|| {
            table_rows.set(vec!["new".to_string()]);
        });
        assert_eq!(
            spans,
            vec![(1, 2)],
            "single-row change should report one row and two invalidated widgets"
        );
    }

    #[test]
    fn schema_change_requires_full_rerender_not_partial_delta() {
        let table = Observable::new(TableSnapshot {
            schema_version: 1,
            rows: vec!["alpha".to_string()],
        });
        let previous = Rc::new(RefCell::new(Some(table.get())));
        let decisions = Rc::new(RefCell::new(Vec::<RenderMode>::new()));
        let previous_clone = Rc::clone(&previous);
        let decisions_clone = Rc::clone(&decisions);

        let _sub = table.subscribe(move |next| {
            let mut prev = previous_clone.borrow_mut();
            let current_mode =
                classify_render_mode(prev.as_ref().expect("previous snapshot available"), next);
            decisions_clone.borrow_mut().push(current_mode);
            *prev = Some(next.clone());
        });

        table.set(TableSnapshot {
            schema_version: 1,
            rows: vec!["alpha".to_string(), "beta".to_string()],
        });
        table.set(TableSnapshot {
            schema_version: 2,
            rows: vec!["alpha".to_string(), "beta".to_string()],
        });

        assert_eq!(
            *decisions.borrow(),
            vec![RenderMode::PartialDelta, RenderMode::FullRerender],
            "schema-version changes must force full rerender semantics"
        );
    }

    #[test]
    fn string_observable() {
        let obs = Observable::new(String::new());
        let changes = Rc::new(Cell::new(0u32));
        let changes_clone = Rc::clone(&changes);

        let _sub = obs.subscribe(move |_| changes_clone.set(changes_clone.get() + 1));

        obs.set("hello".to_string());
        obs.set("hello".to_string()); // Same, no notify.
        obs.set("world".to_string());

        assert_eq!(changes.get(), 2);
        assert_eq!(obs.version(), 2);
    }
}
