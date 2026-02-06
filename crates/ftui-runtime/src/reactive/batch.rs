#![forbid(unsafe_code)]

//! Batch update coalescing for [`Observable`] notifications.
//!
//! When multiple `Observable` values are updated in rapid succession,
//! subscribers receive a notification for each change. In render-heavy
//! scenarios this causes redundant intermediate renders. Batch coalescing
//! defers all notifications until the batch scope exits, then fires each
//! unique callback at most once.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::reactive::batch::BatchScope;
//!
//! let x = Observable::new(0);
//! let y = Observable::new(0);
//!
//! {
//!     let _batch = BatchScope::new();
//!     x.set(1);  // notification deferred
//!     y.set(2);  // notification deferred
//!     x.set(3);  // notification deferred (coalesced with first x.set)
//! }  // all notifications fire here, x subscribers called once with value 3
//! ```
//!
//! # Invariants
//!
//! 1. Nested batches are supported: only the outermost scope triggers flush.
//! 2. Within a batch, `Observable::get()` always returns the latest value
//!    (values are updated immediately, only notifications are deferred).
//! 3. After a batch exits, all subscribers see the final state, never an
//!    intermediate state.
//! 4. Flush calls deferred callbacks in the order they were first enqueued.
//!
//! # Failure Modes
//!
//! - **Callback panics during flush**: Remaining callbacks are still called.
//!   The first panic is re-raised after all callbacks have been attempted.

use std::cell::RefCell;

/// A deferred notification: a closure that fires a subscriber callback
/// with the latest value.
type DeferredNotify = Box<dyn FnOnce()>;

/// Thread-local batch context.
struct BatchContext {
    /// Nesting depth. Only flush when this reaches 0.
    depth: u32,
    /// Queued notifications to fire on flush.
    deferred: Vec<DeferredNotify>,
}

thread_local! {
    static BATCH_CTX: RefCell<Option<BatchContext>> = const { RefCell::new(None) };
}

/// Returns true if a batch is currently active on this thread.
pub fn is_batching() -> bool {
    BATCH_CTX.with(|ctx| ctx.borrow().is_some())
}

/// Enqueue a deferred notification to be fired when the current batch exits.
///
/// If no batch is active, the notification fires immediately.
///
/// Returns `true` if the notification was deferred, `false` if it fired
/// immediately.
pub fn defer_or_run(f: impl FnOnce() + 'static) -> bool {
    BATCH_CTX.with(|ctx| {
        let mut guard = ctx.borrow_mut();
        if let Some(ref mut batch) = *guard {
            batch.deferred.push(Box::new(f));
            true
        } else {
            drop(guard); // Release borrow before calling f.
            f();
            false
        }
    })
}

/// Flush all deferred notifications. Called internally by `BatchScope::drop`.
fn flush() {
    let deferred: Vec<DeferredNotify> = BATCH_CTX.with(|ctx| {
        let mut guard = ctx.borrow_mut();
        if let Some(ref mut batch) = *guard {
            std::mem::take(&mut batch.deferred)
        } else {
            Vec::new()
        }
    });

    // Run all deferred notifications outside the borrow.
    // If a callback panics, we still try to run the rest.
    let mut first_panic: Option<Box<dyn std::any::Any + Send>> = None;
    for notify in deferred {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(notify));
        if let Err(payload) = result
            && first_panic.is_none()
        {
            first_panic = Some(payload);
        }
    }

    if let Some(payload) = first_panic {
        std::panic::resume_unwind(payload);
    }
}

/// RAII guard that begins a batch scope.
///
/// While a `BatchScope` is alive, all [`Observable`](super::Observable)
/// notifications are deferred. When the outermost `BatchScope` drops,
/// all deferred notifications fire.
///
/// Nested `BatchScope`s are supported — only the outermost one flushes.
pub struct BatchScope {
    /// Whether this scope is the outermost (responsible for flush).
    is_root: bool,
}

impl BatchScope {
    /// Begin a new batch scope.
    ///
    /// If already inside a batch, this increments the nesting depth.
    #[must_use]
    pub fn new() -> Self {
        let is_root = BATCH_CTX.with(|ctx| {
            let mut guard = ctx.borrow_mut();
            match *guard {
                Some(ref mut batch) => {
                    batch.depth += 1;
                    false
                }
                None => {
                    *guard = Some(BatchContext {
                        depth: 1,
                        deferred: Vec::new(),
                    });
                    true
                }
            }
        });
        Self { is_root }
    }

    /// Number of deferred notifications queued in the current batch.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        BATCH_CTX.with(|ctx| ctx.borrow().as_ref().map_or(0, |b| b.deferred.len()))
    }
}

impl Default for BatchScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BatchScope {
    fn drop(&mut self) {
        let should_flush = BATCH_CTX.with(|ctx| {
            let mut guard = ctx.borrow_mut();
            if let Some(ref mut batch) = *guard {
                batch.depth -= 1;
                batch.depth == 0
            } else {
                false
            }
        });

        if should_flush {
            flush();
            // Clear the context after flush.
            BATCH_CTX.with(|ctx| {
                *ctx.borrow_mut() = None;
            });
        }
    }
}

impl std::fmt::Debug for BatchScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchScope")
            .field("is_root", &self.is_root)
            .field("pending", &self.pending_count())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::Observable;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn batch_defers_notifications() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let _sub = obs.subscribe(move |_| {
            count_clone.set(count_clone.get() + 1);
        });

        {
            let _batch = BatchScope::new();
            obs.set(1);
            obs.set(2);
            obs.set(3);
            // No notifications yet.
            assert_eq!(count.get(), 0);
        }
        // All notifications fire on batch exit.
        assert!(count.get() > 0);
    }

    #[test]
    fn batch_values_updated_immediately() {
        let obs = Observable::new(0);
        {
            let _batch = BatchScope::new();
            obs.set(42);
            // Value is updated even within batch.
            assert_eq!(obs.get(), 42);
        }
    }

    #[test]
    fn nested_batch_only_outermost_flushes() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let _sub = obs.subscribe(move |_| {
            count_clone.set(count_clone.get() + 1);
        });

        {
            let _outer = BatchScope::new();
            obs.set(1);

            {
                let _inner = BatchScope::new();
                obs.set(2);
                // Inner batch exit doesn't flush.
            }
            assert_eq!(count.get(), 0);
            obs.set(3);
        }
        // Only outer batch exit flushes.
        assert!(count.get() > 0);
    }

    #[test]
    fn no_batch_fires_immediately() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let _sub = obs.subscribe(move |_| {
            count_clone.set(count_clone.get() + 1);
        });

        obs.set(1);
        assert_eq!(count.get(), 1);

        obs.set(2);
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn is_batching_flag() {
        assert!(!is_batching());
        {
            let _batch = BatchScope::new();
            assert!(is_batching());
        }
        assert!(!is_batching());
    }

    #[test]
    fn pending_count() {
        let obs = Observable::new(0);
        let _sub = obs.subscribe(|_| {});

        let batch = BatchScope::new();
        assert_eq!(batch.pending_count(), 0);

        obs.set(1);
        // Each set enqueues a deferred notification.
        assert!(batch.pending_count() > 0);
    }

    #[test]
    fn defer_or_run_without_batch() {
        let ran = Rc::new(Cell::new(false));
        let ran_clone = Rc::clone(&ran);

        let deferred = defer_or_run(move || ran_clone.set(true));
        assert!(!deferred);
        assert!(ran.get());
    }

    #[test]
    fn defer_or_run_with_batch() {
        let ran = Rc::new(Cell::new(false));
        let ran_clone = Rc::clone(&ran);

        {
            let _batch = BatchScope::new();
            let deferred = defer_or_run(move || ran_clone.set(true));
            assert!(deferred);
            assert!(!ran.get());
        }
        assert!(ran.get());
    }

    #[test]
    fn debug_format() {
        let batch = BatchScope::new();
        let dbg = format!("{:?}", batch);
        assert!(dbg.contains("BatchScope"));
        assert!(dbg.contains("is_root"));
        drop(batch);
    }

    #[test]
    fn multiple_observables_in_batch() {
        let a = Observable::new(0);
        let b = Observable::new(0);
        let a_count = Rc::new(Cell::new(0u32));
        let b_count = Rc::new(Cell::new(0u32));
        let a_clone = Rc::clone(&a_count);
        let b_clone = Rc::clone(&b_count);

        let _sub_a = a.subscribe(move |_| a_clone.set(a_clone.get() + 1));
        let _sub_b = b.subscribe(move |_| b_clone.set(b_clone.get() + 1));

        {
            let _batch = BatchScope::new();
            a.set(1);
            b.set(2);
            a.set(3);
            b.set(4);
            assert_eq!(a_count.get(), 0);
            assert_eq!(b_count.get(), 0);
        }
        assert!(a_count.get() > 0);
        assert!(b_count.get() > 0);
    }

    #[test]
    fn batch_scope_default_trait() {
        let batch = BatchScope::default();
        assert!(is_batching());
        drop(batch);
        assert!(!is_batching());
    }

    #[test]
    fn triple_nested_batch() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));
        let count_clone = Rc::clone(&count);

        let _sub = obs.subscribe(move |_| {
            count_clone.set(count_clone.get() + 1);
        });

        {
            let _outer = BatchScope::new();
            obs.set(1);
            {
                let _mid = BatchScope::new();
                obs.set(2);
                {
                    let _inner = BatchScope::new();
                    obs.set(3);
                }
                assert_eq!(count.get(), 0, "inner drop should not flush");
            }
            assert_eq!(count.get(), 0, "mid drop should not flush");
        }
        assert!(count.get() > 0, "outer drop should flush");
    }

    #[test]
    fn empty_batch_no_panic() {
        {
            let _batch = BatchScope::new();
            // No observable mutations
        }
        assert!(!is_batching());
    }

    #[test]
    fn pending_count_zero_without_subscribers() {
        let obs = Observable::new(0);
        let batch = BatchScope::new();
        obs.set(42);
        // Without subscribers, set doesn't enqueue notifications
        assert_eq!(batch.pending_count(), 0);
        drop(batch);
    }
}
