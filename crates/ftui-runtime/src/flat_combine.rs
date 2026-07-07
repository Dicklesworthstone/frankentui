//! Caller-driven flat combining for batched operation dispatch.
//!
//! When multiple event sources (timers, background tasks, input) post
//! operations concurrently, submitters enqueue them under a short queue
//! lock and whichever thread calls [`FlatCombiner::combine`] acts as the
//! combiner: it drains the queue and executes ALL pending operations in
//! one pass while holding the state lock, keeping data hot in L1 cache.
//!
//! Unlike classic flat combining, there is no combiner election and
//! submitters do not wait for results: operations are fire-and-forget,
//! and nothing runs until some thread explicitly polls `combine()` (or
//! `combine_with`). Operations still queued when the `FlatCombiner` is
//! dropped are discarded, and the publication queue is unbounded — the
//! polling cadence is the backpressure.
//!
//! # When to Use
//!
//! Use this instead of a bare `Mutex` when:
//! - Multiple threads/tasks post operations to shared state
//! - A natural polling point exists (e.g., once per frame/tick)
//! - Operations are short (the combiner shouldn't hold the lock too long)
//! - Batching is beneficial (e.g., coalescing events, reducing redraws)
//!
//! # Example
//!
//! ```
//! use ftui_runtime::flat_combine::FlatCombiner;
//!
//! let combiner = FlatCombiner::new(Vec::<String>::new());
//!
//! // Submit operations (from any thread)
//! combiner.submit(|state| state.push("event-a".into()));
//! combiner.submit(|state| state.push("event-b".into()));
//!
//! // Combiner drains and applies all pending ops in one pass
//! let count = combiner.combine();
//! assert_eq!(count, 2);
//!
//! // Direct execution when no contention
//! let len = combiner.execute(|state| state.len());
//! assert_eq!(len, 2);
//! ```

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::ThreadId;

/// Statistics for monitoring flat combining performance.
#[derive(Debug, Clone, Default)]
pub struct CombinerStats {
    /// Number of combine passes executed.
    pub combine_passes: u64,
    /// Total operations processed across all passes.
    pub total_ops: u64,
    /// Maximum batch size seen in a single pass.
    pub max_batch_size: usize,
    /// Number of times a submitter found the queue locked (contention signal).
    pub contention_events: u64,
}

impl CombinerStats {
    /// Average batch size across all combine passes.
    pub fn avg_batch_size(&self) -> f64 {
        if self.combine_passes == 0 {
            0.0
        } else {
            self.total_ops as f64 / self.combine_passes as f64
        }
    }
}

/// Flat combining dispatcher for batched operation execution.
///
/// Wraps shared mutable state with a two-level locking strategy:
/// 1. A publication queue (`queue`) where threads post operations
/// 2. The shared state (`state`) where operations are executed
///
/// The combiner thread locks the state once, drains the queue, and
/// executes all operations in sequence — keeping the hot data in cache
/// and minimizing lock handoffs.
pub struct FlatCombiner<S> {
    /// Protected shared state.
    state: Mutex<S>,
    /// Publication queue for pending operations.
    queue: Mutex<Vec<BoxedOp<S>>>,
    /// Monotonic generation counter (incremented after each combine pass).
    generation: AtomicU64,
    /// Performance statistics.
    stats: Mutex<CombinerStats>,
    /// Owner thread while a combine pass (`combine` or `combine_with`) is
    /// actively executing user callbacks under the state lock.
    combine_owner: Mutex<Option<ThreadId>>,
}

type BoxedOp<S> = Box<dyn FnOnce(&mut S) + Send>;

struct CombineOwnerGuard<'a> {
    owner: &'a Mutex<Option<ThreadId>>,
}

impl Drop for CombineOwnerGuard<'_> {
    fn drop(&mut self) {
        let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        *owner = None;
    }
}

impl<'a> CombineOwnerGuard<'a> {
    fn new(owner: &'a Mutex<Option<ThreadId>>) -> Self {
        let current = std::thread::current().id();
        let mut owner_guard = owner.lock().unwrap_or_else(|e| e.into_inner());
        *owner_guard = Some(current);
        drop(owner_guard);
        Self { owner }
    }
}

impl<S> FlatCombiner<S> {
    /// Create a new flat combiner wrapping the given shared state.
    pub fn new(state: S) -> Self {
        Self {
            state: Mutex::new(state),
            queue: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            stats: Mutex::new(CombinerStats::default()),
            combine_owner: Mutex::new(None),
        }
    }

    fn assert_not_reentrant(&self, operation: &str) {
        let current = std::thread::current().id();
        let owner = self.combine_owner.lock().unwrap_or_else(|e| e.into_inner());
        if owner
            .as_ref()
            .is_some_and(|thread_id| *thread_id == current)
        {
            panic!("FlatCombiner::{operation} cannot be called reentrantly from a combine pass");
        }
    }

    /// Lock the publication queue, recording a contention event when the
    /// lock is currently held by another thread.
    fn lock_queue(&self) -> std::sync::MutexGuard<'_, Vec<BoxedOp<S>>> {
        match self.queue.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if let Ok(mut stats) = self.stats.lock() {
                    stats.contention_events += 1;
                }
                self.queue.lock().unwrap_or_else(|e| e.into_inner())
            }
        }
    }

    /// Execute a single operation directly on the shared state.
    ///
    /// Bypasses the publication queue. Use this when you need a return
    /// value or when contention is not expected.
    pub fn execute<R>(&self, op: impl FnOnce(&mut S) -> R) -> R {
        self.assert_not_reentrant("execute");
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        op(&mut state)
    }

    /// Read from the shared state without mutation.
    pub fn with_state<R>(&self, f: impl FnOnce(&S) -> R) -> R {
        self.assert_not_reentrant("with_state");
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&state)
    }

    /// Submit an operation to the publication queue for batched execution.
    ///
    /// The operation will be executed during the next [`combine`](Self::combine)
    /// call. Operations are executed in submission order within each batch.
    /// Submitting from inside a combine pass is allowed; the operation lands
    /// in the next batch.
    pub fn submit(&self, op: impl FnOnce(&mut S) + Send + 'static) {
        let mut queue = self.lock_queue();
        queue.push(Box::new(op));
    }

    /// Submit multiple operations at once (avoids repeated lock acquisitions).
    pub fn submit_batch(&self, ops: impl IntoIterator<Item = BoxedOp<S>>) {
        let mut queue = self.lock_queue();
        queue.extend(ops);
    }

    /// Drain all pending operations and execute them as a single batch.
    ///
    /// The combiner holds the state lock for the entire batch, keeping
    /// the data hot in L1 cache. Returns the number of operations executed.
    ///
    /// Returns 0 if no operations are pending.
    ///
    /// Reentrant calls back into [`execute`](Self::execute),
    /// [`with_state`](Self::with_state), [`combine`](Self::combine), or
    /// [`combine_with`](Self::combine_with) from inside an operation are
    /// rejected with a panic instead of deadlocking on the state mutex
    /// ([`submit`](Self::submit) is fine). If an operation panics, the
    /// remaining operations of the drained batch are dropped and the
    /// generation/stats are not updated.
    pub fn combine(&self) -> usize {
        self.assert_not_reentrant("combine");
        // Drain the queue (short lock)
        let ops: Vec<BoxedOp<S>> = {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *queue)
        };

        if ops.is_empty() {
            return 0;
        }

        let count = ops.len();

        // Execute all operations (holds state lock for entire batch)
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let _owner_guard = CombineOwnerGuard::new(&self.combine_owner);
            for op in ops {
                op(&mut state);
            }
        }

        // Update stats and generation
        self.generation.fetch_add(1, Ordering::Release);
        if let Ok(mut stats) = self.stats.lock() {
            stats.combine_passes += 1;
            stats.total_ops += count as u64;
            stats.max_batch_size = stats.max_batch_size.max(count);
        }

        count
    }

    /// Combine with a pre/post hook for additional work during the batch.
    ///
    /// The `around` function receives a mutable reference to the state
    /// and a closure that executes all pending operations. This allows
    /// wrapping the batch with setup/teardown logic (e.g., marking a
    /// dirty flag, snapshotting state). The batch is applied exactly once:
    /// if `around` returns without invoking the closure, the pending
    /// operations are executed immediately after it returns (still under
    /// the state lock) — they are never silently dropped.
    ///
    /// Reentrant calls back into [`execute`](Self::execute),
    /// [`with_state`](Self::with_state), [`combine`](Self::combine), or
    /// [`combine_with`](Self::combine_with) from inside `around` are rejected
    /// with a panic instead of deadlocking on the state mutex. If `around`
    /// or an operation panics, any not-yet-executed drained operations are
    /// dropped and the generation/stats are not updated.
    pub fn combine_with<R>(&self, around: impl FnOnce(&mut S, &dyn Fn(&mut S)) -> R) -> (usize, R) {
        self.assert_not_reentrant("combine_with");
        let ops: Vec<BoxedOp<S>> = {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *queue)
        };

        let count = ops.len();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let _owner_guard = CombineOwnerGuard::new(&self.combine_owner);

        // We need to move ops into the closure, but the Fn trait requires
        // shared reference. Use a Cell-like approach with RefCell.
        let ops_cell = std::cell::RefCell::new(Some(ops));
        let apply = |s: &mut S| {
            if let Some(ops) = ops_cell.borrow_mut().take() {
                for op in ops {
                    op(s);
                }
            }
        };

        let result = around(&mut state, &apply);
        // Exactly-once guarantee: if `around` never called `apply`, run the
        // drained batch now instead of dropping it (idempotent: `apply`
        // consumes the ops on first invocation).
        apply(&mut state);

        if count > 0 {
            self.generation.fetch_add(1, Ordering::Release);
            if let Ok(mut stats) = self.stats.lock() {
                stats.combine_passes += 1;
                stats.total_ops += count as u64;
                stats.max_batch_size = stats.max_batch_size.max(count);
            }
        }

        (count, result)
    }

    /// Number of operations currently in the publication queue.
    pub fn pending_count(&self) -> usize {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Current generation counter. Incremented after each combine pass.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Get a snapshot of current performance statistics.
    pub fn stats(&self) -> CombinerStats {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Reset statistics counters.
    pub fn reset_stats(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            *stats = CombinerStats::default();
        }
    }
}

// FlatCombiner is Send + Sync if S is Send (the Mutex handles the synchronization)
// This is automatically derived by the compiler since all fields are Send + Sync.

impl<S: std::fmt::Debug> std::fmt::Debug for FlatCombiner<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self.pending_count();
        let current_gen = self.generation();
        f.debug_struct("FlatCombiner")
            .field("pending", &pending)
            .field("generation", &current_gen)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn new_creates_empty_combiner() {
        let fc = FlatCombiner::new(0u64);
        assert_eq!(fc.pending_count(), 0);
        assert_eq!(fc.generation(), 0);
        assert_eq!(fc.stats().combine_passes, 0);
    }

    #[test]
    fn execute_applies_directly() {
        let fc = FlatCombiner::new(10u64);
        let result = fc.execute(|s| {
            *s += 5;
            *s
        });
        assert_eq!(result, 15);
    }

    #[test]
    fn with_state_reads_without_mutation() {
        let fc = FlatCombiner::new(vec![1, 2, 3]);
        let len = fc.with_state(|s| s.len());
        assert_eq!(len, 3);
    }

    #[test]
    fn submit_queues_operations() {
        let fc = FlatCombiner::new(0u64);
        fc.submit(|s| *s += 1);
        fc.submit(|s| *s += 2);
        assert_eq!(fc.pending_count(), 2);

        // State not yet modified
        let val = fc.with_state(|s| *s);
        assert_eq!(val, 0);
    }

    #[test]
    fn combine_drains_and_applies() {
        let fc = FlatCombiner::new(0u64);
        fc.submit(|s| *s += 10);
        fc.submit(|s| *s += 20);
        fc.submit(|s| *s += 30);

        let count = fc.combine();
        assert_eq!(count, 3);
        assert_eq!(fc.pending_count(), 0);

        let val = fc.with_state(|s| *s);
        assert_eq!(val, 60);
    }

    #[test]
    fn combine_empty_returns_zero() {
        let fc = FlatCombiner::new(0u64);
        assert_eq!(fc.combine(), 0);
        assert_eq!(fc.generation(), 0);
    }

    #[test]
    fn combine_increments_generation() {
        let fc = FlatCombiner::new(0u64);
        assert_eq!(fc.generation(), 0);

        fc.submit(|s| *s += 1);
        fc.combine();
        assert_eq!(fc.generation(), 1);

        fc.submit(|s| *s += 1);
        fc.combine();
        assert_eq!(fc.generation(), 2);
    }

    #[test]
    fn stats_track_batches() {
        let fc = FlatCombiner::new(0u64);

        // Batch 1: 3 ops
        fc.submit(|s| *s += 1);
        fc.submit(|s| *s += 1);
        fc.submit(|s| *s += 1);
        fc.combine();

        // Batch 2: 1 op
        fc.submit(|s| *s += 1);
        fc.combine();

        let stats = fc.stats();
        assert_eq!(stats.combine_passes, 2);
        assert_eq!(stats.total_ops, 4);
        assert_eq!(stats.max_batch_size, 3);
        assert!((stats.avg_batch_size() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reset_stats_clears_counters() {
        let fc = FlatCombiner::new(0u64);
        fc.submit(|s| *s += 1);
        fc.combine();
        assert_eq!(fc.stats().combine_passes, 1);

        fc.reset_stats();
        let stats = fc.stats();
        assert_eq!(stats.combine_passes, 0);
        assert_eq!(stats.total_ops, 0);
    }

    #[test]
    fn operations_execute_in_order() {
        let fc = FlatCombiner::new(Vec::<u32>::new());
        fc.submit(|s| s.push(1));
        fc.submit(|s| s.push(2));
        fc.submit(|s| s.push(3));
        fc.combine();

        let values = fc.with_state(|s| s.clone());
        assert_eq!(values, vec![1, 2, 3]);
    }

    #[test]
    fn submit_batch_adds_multiple() {
        let fc = FlatCombiner::new(0u64);
        let ops: Vec<BoxedOp<u64>> = vec![
            Box::new(|s: &mut u64| *s += 10),
            Box::new(|s: &mut u64| *s += 20),
        ];
        fc.submit_batch(ops);
        assert_eq!(fc.pending_count(), 2);
        fc.combine();
        assert_eq!(fc.with_state(|s| *s), 30);
    }

    #[test]
    fn combine_with_wraps_batch() {
        let fc = FlatCombiner::new(Vec::<String>::new());
        fc.submit(|s| s.push("a".into()));
        fc.submit(|s| s.push("b".into()));

        let (count, len_before) = fc.combine_with(|state, apply| {
            let before = state.len();
            apply(state);
            before
        });

        assert_eq!(count, 2);
        assert_eq!(len_before, 0);
        assert_eq!(fc.with_state(|s| s.len()), 2);
    }

    #[test]
    fn multiple_combine_passes() {
        let fc = FlatCombiner::new(0u64);

        for i in 0..10 {
            fc.submit(move |s| *s += i);
        }
        fc.combine();
        assert_eq!(fc.with_state(|s| *s), 45); // sum 0..10

        for i in 0..5 {
            fc.submit(move |s| *s += i);
        }
        fc.combine();
        assert_eq!(fc.with_state(|s| *s), 55); // 45 + sum 0..5
    }

    #[test]
    fn debug_impl() {
        let fc = FlatCombiner::new(42u64);
        let debug = format!("{fc:?}");
        assert!(debug.contains("FlatCombiner"));
        assert!(debug.contains("pending"));
        assert!(debug.contains("generation"));
    }

    #[test]
    fn concurrent_submit_and_combine() {
        let fc = Arc::new(FlatCombiner::new(0u64));

        // Spawn threads that submit operations
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let fc = Arc::clone(&fc);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        fc.submit(|s| *s += 1);
                    }
                })
            })
            .collect();

        // Wait for all submitters
        for h in handles {
            h.join().unwrap();
        }

        // Combine all pending operations
        let mut total = 0;
        loop {
            let count = fc.combine();
            if count == 0 {
                break;
            }
            total += count;
        }

        assert_eq!(total, 800);
        assert_eq!(fc.with_state(|s| *s), 800);
    }

    #[test]
    fn concurrent_submit_and_combine_interleaved() {
        let fc = Arc::new(FlatCombiner::new(0u64));

        // Submitter threads
        let submit_handles: Vec<_> = (0..4)
            .map(|_| {
                let fc = Arc::clone(&fc);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        fc.submit(|s| *s += 1);
                        std::thread::yield_now();
                    }
                })
            })
            .collect();

        // Combiner thread
        let fc_c = Arc::clone(&fc);
        let combiner = std::thread::spawn(move || {
            let mut total = 0;
            for _ in 0..500 {
                total += fc_c.combine();
                std::thread::yield_now();
            }
            total
        });

        for h in submit_handles {
            h.join().unwrap();
        }

        // Drain remaining
        let combined_during = combiner.join().unwrap();
        let remaining = fc.combine();
        let final_val = fc.with_state(|s| *s);

        assert_eq!(
            final_val,
            (combined_during + remaining) as u64,
            "total combined ({} + {}) should match state ({})",
            combined_during,
            remaining,
            final_val
        );
        assert_eq!(final_val, 400);
    }

    #[test]
    fn poison_recovery() {
        // A panicking operation poisons the state mutex mid-combine; every
        // subsequent entry point must recover via `into_inner` and keep
        // working.
        let fc = FlatCombiner::new(0u64);
        fc.submit(|_| panic!("op panics"));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fc.combine()));
        assert!(result.is_err(), "panic must propagate out of combine");
        assert_eq!(fc.generation(), 0, "aborted pass must not bump generation");

        fc.execute(|s| *s += 1);
        fc.submit(|s| *s += 1);
        assert_eq!(fc.combine(), 1);
        assert_eq!(fc.with_state(|s| *s), 2);
        assert_eq!(fc.generation(), 1);
    }

    #[test]
    fn combine_panics_on_reentrant_call_from_op_instead_of_deadlocking() {
        let fc = Arc::new(FlatCombiner::new(0u64));
        let fc2 = Arc::clone(&fc);
        fc.submit(move |_| {
            let _ = fc2.with_state(|s| *s);
        });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fc.combine()));
        assert!(result.is_err(), "reentrant with_state must panic, not hang");
    }

    #[test]
    fn submit_from_inside_combine_op_lands_in_next_batch() {
        let fc = Arc::new(FlatCombiner::new(0u64));
        let fc2 = Arc::clone(&fc);
        fc.submit(move |s| {
            *s += 1;
            fc2.submit(|s| *s += 10);
        });

        assert_eq!(fc.combine(), 1);
        assert_eq!(fc.with_state(|s| *s), 1);
        assert_eq!(fc.pending_count(), 1);
        assert_eq!(fc.combine(), 1);
        assert_eq!(fc.with_state(|s| *s), 11);
    }

    #[test]
    fn combine_with_executes_batch_even_if_apply_not_called() {
        let fc = FlatCombiner::new(0u64);
        fc.submit(|s| *s += 5);

        let (count, ()) = fc.combine_with(|_, _apply| ());

        assert_eq!(count, 1);
        assert_eq!(
            fc.with_state(|s| *s),
            5,
            "batch must be applied even when `around` skips `apply`"
        );
        assert_eq!(fc.generation(), 1);
    }

    #[test]
    fn contention_events_recorded_when_queue_is_held() {
        let fc = Arc::new(FlatCombiner::new(0u64));

        // Hold the publication queue so the submitter thread's try_lock
        // fails deterministically.
        let queue_guard = fc.queue.lock().unwrap();
        let fc2 = Arc::clone(&fc);
        let submitter = std::thread::spawn(move || fc2.submit(|s| *s += 1));

        // The submitter records the contention event before blocking on the
        // queue lock, so this loop terminates without releasing the queue.
        while fc.stats().contention_events == 0 {
            std::thread::yield_now();
        }
        drop(queue_guard);
        submitter.join().unwrap();

        assert!(fc.stats().contention_events >= 1);
        assert_eq!(fc.combine(), 1);
        assert_eq!(fc.with_state(|s| *s), 1);
    }

    #[test]
    fn avg_batch_size_zero_when_no_combines() {
        let stats = CombinerStats::default();
        assert_eq!(stats.avg_batch_size(), 0.0);
    }

    #[test]
    fn combine_with_panics_on_reentrant_execute_instead_of_deadlocking() {
        let fc = FlatCombiner::new(0u64);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fc.combine_with(|_, _| fc.execute(|state| *state));
        }));

        assert!(result.is_err());
    }

    #[test]
    fn combine_with_panics_on_reentrant_with_state_instead_of_deadlocking() {
        let fc = FlatCombiner::new(7u64);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fc.combine_with(|_, _| fc.with_state(|state| *state));
        }));

        assert!(result.is_err());
    }
}
