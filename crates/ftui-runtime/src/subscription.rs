#![forbid(unsafe_code)]

//! Subscription system for continuous event sources.
//!
//! Subscriptions provide a declarative way to receive events from external
//! sources like timers, file watchers, or network connections. The runtime
//! manages subscription lifecycles automatically based on what the model
//! declares as active.
//!
//! # How it works
//!
//! 1. `Model::subscriptions()` returns the set of active subscriptions
//! 2. After each `update()`, the runtime compares active vs previous subscriptions
//! 3. New subscriptions are started, removed ones are stopped
//! 4. Subscription messages are routed through `Model::update()`

use crate::cancellation::{CancellationSource, CancellationToken};
use std::collections::HashSet;
use std::sync::mpsc;
use std::thread;
use web_time::{Duration, Instant};

/// A unique identifier for a subscription.
///
/// Used by the runtime to track which subscriptions are active and
/// to deduplicate subscriptions across update cycles.
pub type SubId = u64;

/// A subscription produces messages from an external event source.
///
/// Subscriptions run on background threads and send messages through
/// the provided channel. The runtime manages their lifecycle.
pub trait Subscription<M: Send + 'static>: Send {
    /// Unique identifier for deduplication.
    ///
    /// Subscriptions with the same ID are considered identical.
    /// The runtime uses this to avoid restarting unchanged subscriptions.
    fn id(&self) -> SubId;

    /// Start the subscription, sending messages through the channel.
    ///
    /// This is called on a background thread. Implementations should
    /// loop and send messages until the channel is disconnected (receiver dropped)
    /// or the stop signal is received.
    fn run(&self, sender: mpsc::Sender<M>, stop: StopSignal);
}

/// Signal for stopping a subscription.
///
/// When the runtime stops a subscription, it sets this signal. The subscription
/// should check it periodically and exit its run loop when set.
///
/// Backed by [`CancellationToken`] for structured cancellation.
#[derive(Clone)]
pub struct StopSignal {
    token: CancellationToken,
}

impl StopSignal {
    /// Create a new stop signal pair (signal, trigger).
    pub(crate) fn new() -> (Self, StopTrigger) {
        let source = CancellationSource::new();
        let signal = Self {
            token: source.token(),
        };
        let trigger = StopTrigger { source };
        (signal, trigger)
    }

    /// Check if the stop signal has been triggered.
    pub fn is_stopped(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Wait for either the stop signal or a timeout.
    ///
    /// Returns `true` if stopped, `false` if timed out.
    /// Blocks the thread efficiently using a condition variable.
    /// Handles spurious wakeups by looping until condition met or timeout expired.
    pub fn wait_timeout(&self, duration: Duration) -> bool {
        self.token.wait_timeout(duration)
    }

    /// Access the underlying cancellation token.
    ///
    /// This enables integration with Asupersync-style structured cancellation
    /// while preserving backwards compatibility with the `StopSignal` API.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.token
    }
}

/// Trigger to stop a subscription from the runtime side.
///
/// Backed by [`CancellationSource`] for structured cancellation.
pub(crate) struct StopTrigger {
    source: CancellationSource,
}

impl StopTrigger {
    /// Signal the subscription to stop.
    pub(crate) fn stop(&self) {
        self.source.cancel();
    }
}

/// A running subscription handle.
pub(crate) struct RunningSubscription {
    pub(crate) id: SubId,
    trigger: StopTrigger,
    thread: Option<thread::JoinHandle<()>>,
    /// Tracks whether the subscription thread panicked (set by the catch_unwind wrapper).
    panicked: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A background subscription failure waiting to be reported to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscriptionFailure {
    pub(crate) id: SubId,
    pub(crate) error: String,
}

const SUBSCRIPTION_STOP_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
/// Poll interval for bounded subscription thread joins (bd-1f2aw).
///
/// Same rationale as the executor shutdown polls: `JoinHandle` has no
/// `join_timeout` in stable Rust, so we poll `is_finished()` with a short
/// sleep. 1ms minimizes stop latency while avoiding spin.
const SUBSCRIPTION_STOP_JOIN_POLL: Duration = Duration::from_millis(1);

impl RunningSubscription {
    /// Returns true if the subscription thread panicked.
    pub(crate) fn has_panicked(&self) -> bool {
        self.panicked.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Signal the subscription to stop (phase 1 of two-phase shutdown).
    ///
    /// Does NOT join the thread — call [`join_bounded`] after signalling all
    /// subscriptions to allow parallel wind-down (bd-1f2aw).
    pub(crate) fn signal_stop(&self) {
        self.trigger.stop();
    }

    /// Join the subscription thread with a bounded timeout (phase 2).
    ///
    /// Returns the join handle if the thread did not finish within the timeout,
    /// allowing callers to log and move on without blocking indefinitely.
    pub(crate) fn join_bounded(mut self) -> Option<thread::JoinHandle<()>> {
        let handle = self.thread.take()?;
        let start = Instant::now();

        // Fast path: subscription already finished (common for short-lived subs).
        if handle.is_finished() {
            let _ = handle.join();
            tracing::trace!(
                sub_id = self.id,
                panicked = self.has_panicked(),
                elapsed_us = start.elapsed().as_micros() as u64,
                "subscription join (fast path)"
            );
            return None;
        }

        // Slow path: bounded poll loop (bd-1f2aw).
        while !handle.is_finished() {
            if start.elapsed() >= SUBSCRIPTION_STOP_JOIN_TIMEOUT {
                tracing::warn!(
                    sub_id = self.id,
                    panicked = self.has_panicked(),
                    timeout_ms = SUBSCRIPTION_STOP_JOIN_TIMEOUT.as_millis() as u64,
                    "subscription join timed out, detaching thread"
                );
                return Some(handle);
            }
            thread::sleep(SUBSCRIPTION_STOP_JOIN_POLL);
        }

        let _ = handle.join();
        tracing::trace!(
            sub_id = self.id,
            panicked = self.has_panicked(),
            elapsed_us = start.elapsed().as_micros() as u64,
            "subscription join (slow path)"
        );
        None
    }

    /// Stop the subscription and join its thread if it exits promptly.
    ///
    /// Convenience method combining signal + join for single-subscription stops.
    /// Used by tests and external callers that stop a single subscription.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn stop(mut self) {
        self.trigger.stop();
        if let Some(handle) = self.thread.take() {
            let start = Instant::now();
            // Fast path: subscription already finished (common for short-lived subs).
            if handle.is_finished() {
                let _ = handle.join();
                tracing::trace!(
                    sub_id = self.id,
                    panicked = self.has_panicked(),
                    elapsed_us = start.elapsed().as_micros() as u64,
                    "subscription stop (fast path)"
                );
                return;
            }
            // Slow path: bounded poll loop (bd-1f2aw).
            while !handle.is_finished() {
                if start.elapsed() >= SUBSCRIPTION_STOP_JOIN_TIMEOUT {
                    tracing::warn!(
                        sub_id = self.id,
                        panicked = self.has_panicked(),
                        timeout_ms = SUBSCRIPTION_STOP_JOIN_TIMEOUT.as_millis() as u64,
                        "subscription did not stop within timeout; detaching thread"
                    );
                    return;
                }
                thread::sleep(SUBSCRIPTION_STOP_JOIN_POLL);
            }
            let _ = handle.join();
            tracing::trace!(
                sub_id = self.id,
                panicked = self.has_panicked(),
                elapsed_us = start.elapsed().as_micros() as u64,
                "subscription stop (slow path)"
            );
        }
    }
}

impl Drop for RunningSubscription {
    fn drop(&mut self) {
        self.trigger.stop();
        // Don't join in drop to avoid blocking
    }
}

/// Manages the lifecycle of subscriptions for a program.
pub(crate) struct SubscriptionManager<M: Send + 'static> {
    active: Vec<RunningSubscription>,
    sender: mpsc::Sender<M>,
    receiver: mpsc::Receiver<M>,
    failure_sender: mpsc::Sender<SubscriptionFailure>,
    failure_receiver: mpsc::Receiver<SubscriptionFailure>,
}

impl<M: Send + 'static> SubscriptionManager<M> {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        let (failure_sender, failure_receiver) = mpsc::channel();
        Self {
            active: Vec::new(),
            sender,
            receiver,
            failure_sender,
            failure_receiver,
        }
    }

    /// Update the set of active subscriptions.
    ///
    /// Compares the new set against currently running subscriptions:
    /// - Starts subscriptions that are new (ID not in active set)
    /// - Stops subscriptions that are no longer declared (ID not in new set)
    /// - Leaves unchanged subscriptions running
    pub(crate) fn reconcile(&mut self, subscriptions: Vec<Box<dyn Subscription<M>>>) {
        let reconcile_start = Instant::now();
        let new_ids: HashSet<SubId> = subscriptions.iter().map(|s| s.id()).collect();
        let active_count_before = self.active.len();

        crate::debug_trace!(
            "reconcile: new_ids={:?}, active_before={}",
            new_ids,
            active_count_before
        );
        tracing::trace!(
            new_id_count = new_ids.len(),
            active_before = active_count_before,
            new_ids = ?new_ids,
            "subscription reconcile starting"
        );

        // Stop subscriptions that are no longer active (two-phase: bd-1f2aw).
        let mut remaining = Vec::new();
        let mut to_stop = Vec::new();
        for running in self.active.drain(..) {
            if new_ids.contains(&running.id) {
                remaining.push(running);
            } else {
                crate::debug_trace!("stopping subscription: id={}", running.id);
                tracing::debug!(sub_id = running.id, "Stopping subscription");
                crate::effect_system::record_subscription_stop("subscription", running.id, 0);
                crate::effect_system::record_dynamics_sub_stop();
                to_stop.push(running);
            }
        }
        // Phase 1: Signal all removals.
        for running in &to_stop {
            running.signal_stop();
        }
        let stopped_count = to_stop.len();
        // Phase 2: Join with bounded timeout.
        for running in to_stop {
            let _ = running.join_bounded();
        }
        self.active = remaining;

        // Start new subscriptions
        let mut started_count = 0usize;
        let mut active_ids: HashSet<SubId> = self.active.iter().map(|r| r.id).collect();
        for sub in subscriptions {
            let id = sub.id();
            if !active_ids.insert(id) {
                continue;
            }
            started_count += 1;

            crate::debug_trace!("starting subscription: id={}", id);
            tracing::debug!(sub_id = id, "Starting subscription");
            crate::effect_system::record_subscription_start("subscription", id);
            crate::effect_system::record_dynamics_sub_start();
            let (signal, trigger) = StopSignal::new();
            let sender = self.sender.clone();
            let panicked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let panicked_flag = panicked.clone();
            let sub_id_for_thread = id;
            let failure_sender = self.failure_sender.clone();

            let thread = thread::spawn(move || {
                // Recoverable boundary: a panicking subscription is recorded
                // and the program continues. Suppress the terminal panic
                // hook so it doesn't tear down live terminal state mid-run.
                let result = ftui_core::with_panic_cleanup_suppressed(|| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        sub.run(sender, signal);
                    }))
                });
                if let Err(payload) = result {
                    panicked_flag.store(true, std::sync::atomic::Ordering::Release);
                    crate::effect_system::record_dynamics_sub_panic();
                    let panic_msg = match payload.downcast_ref::<&str>() {
                        Some(s) => (*s).to_string(),
                        None => match payload.downcast_ref::<String>() {
                            Some(s) => s.clone(),
                            None => "unknown panic payload".to_string(),
                        },
                    };
                    crate::effect_system::error_effect_panic(
                        "subscription",
                        &format!("sub_id={sub_id_for_thread}: {panic_msg}"),
                    );
                    let _ = failure_sender.send(SubscriptionFailure {
                        id: sub_id_for_thread,
                        error: panic_msg,
                    });
                }
            });

            self.active.push(RunningSubscription {
                id,
                trigger,
                thread: Some(thread),
                panicked,
            });
        }

        let active_count_after = self.active.len();
        let reconcile_elapsed_us = reconcile_start.elapsed().as_micros() as u64;
        crate::effect_system::record_dynamics_reconcile(reconcile_elapsed_us);
        crate::debug_trace!("reconcile complete: active_after={}", active_count_after);
        tracing::trace!(
            active_before = active_count_before,
            active_after = active_count_after,
            started = started_count,
            stopped = stopped_count,
            reconcile_us = reconcile_elapsed_us,
            "subscription reconcile complete"
        );
    }

    /// Drain pending messages from subscriptions.
    pub(crate) fn drain_messages(&self) -> Vec<M> {
        let mut messages = Vec::new();
        while let Ok(msg) = self.receiver.try_recv() {
            messages.push(msg);
        }
        messages
    }

    /// Drain subscription failures that have not yet been reported to the model.
    pub(crate) fn drain_failures(&self) -> Vec<SubscriptionFailure> {
        let mut failures = Vec::new();
        while let Ok(failure) = self.failure_receiver.try_recv() {
            failures.push(failure);
        }
        failures
    }

    /// Return the number of active subscriptions.
    #[inline]
    pub(crate) fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Stop all running subscriptions using two-phase parallel shutdown (bd-1f2aw).
    ///
    /// Phase 1: Signal all subscriptions to stop (non-blocking).
    /// Phase 2: Join all threads with bounded timeout.
    ///
    /// This is significantly faster than sequential stop when multiple
    /// subscriptions are active, because all threads begin winding down
    /// simultaneously rather than waiting for each to finish in turn.
    pub(crate) fn stop_all(&mut self) {
        let count = self.active.len();
        if count == 0 {
            return;
        }
        let start = Instant::now();

        // Phase 1: Signal all subscriptions to stop (parallel).
        for running in &self.active {
            running.signal_stop();
        }

        let signal_elapsed_us = start.elapsed().as_micros() as u64;
        tracing::trace!(
            target: "ftui.runtime",
            count,
            signal_elapsed_us,
            "subscription stop_all phase 1 (signal) complete"
        );

        // Phase 2: Join all threads with bounded timeout.
        let mut panicked_count = 0_usize;
        let mut timed_out_count = 0_usize;
        for running in self.active.drain(..) {
            if running.has_panicked() {
                panicked_count += 1;
            }
            if running.join_bounded().is_some() {
                timed_out_count += 1;
            }
        }

        let shutdown_elapsed_us = start.elapsed().as_micros() as u64;
        crate::effect_system::record_dynamics_shutdown(shutdown_elapsed_us, timed_out_count as u64);
        tracing::debug!(
            target: "ftui.runtime",
            count,
            panicked_count,
            timed_out_count,
            elapsed_us = shutdown_elapsed_us,
            "subscription stop_all complete"
        );
    }
}

impl<M: Send + 'static> Drop for SubscriptionManager<M> {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// --- Built-in subscriptions ---

/// A subscription that fires at a fixed interval.
///
/// # Example
///
/// ```ignore
/// fn subscriptions(&self) -> Vec<Box<dyn Subscription<MyMsg>>> {
///     vec![Box::new(Every::new(Duration::from_secs(1), || MyMsg::Tick))]
/// }
/// ```
pub struct Every<M: Send + 'static> {
    id: SubId,
    interval: Duration,
    make_msg: Box<dyn Fn() -> M + Send + Sync>,
}

impl<M: Send + 'static> Every<M> {
    /// Create a tick subscription with the given interval and message factory.
    pub fn new(interval: Duration, make_msg: impl Fn() -> M + Send + Sync + 'static) -> Self {
        // Generate a stable ID from the interval to allow deduplication
        let id = interval.as_nanos() as u64 ^ 0x5449_434B; // "TICK" magic
        Self {
            id,
            interval,
            make_msg: Box::new(make_msg),
        }
    }

    /// Create a tick subscription with an explicit ID.
    pub fn with_id(
        id: SubId,
        interval: Duration,
        make_msg: impl Fn() -> M + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            interval,
            make_msg: Box::new(make_msg),
        }
    }
}

impl<M: Send + 'static> Subscription<M> for Every<M> {
    fn id(&self) -> SubId {
        self.id
    }

    fn run(&self, sender: mpsc::Sender<M>, stop: StopSignal) {
        let mut tick_count: u64 = 0;
        crate::debug_trace!(
            "Every subscription started: id={}, interval={:?}",
            self.id,
            self.interval
        );
        loop {
            if stop.wait_timeout(self.interval) {
                crate::debug_trace!(
                    "Every subscription stopped: id={}, sent {} ticks",
                    self.id,
                    tick_count
                );
                break;
            }
            tick_count += 1;
            let msg = (self.make_msg)();
            if sender.send(msg).is_err() {
                crate::debug_trace!(
                    "Every subscription channel closed: id={}, sent {} ticks",
                    self.id,
                    tick_count
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum TestMsg {
        Tick,
        Value(i32),
    }

    struct ChannelSubscription<M: Send + 'static> {
        id: SubId,
        receiver: mpsc::Receiver<M>,
        poll: Duration,
    }

    impl<M: Send + 'static> ChannelSubscription<M> {
        fn new(id: SubId, receiver: mpsc::Receiver<M>) -> Self {
            Self {
                id,
                receiver,
                poll: Duration::from_millis(5),
            }
        }
    }

    impl<M: Send + 'static> Subscription<M> for ChannelSubscription<M> {
        fn id(&self) -> SubId {
            self.id
        }

        fn run(&self, sender: mpsc::Sender<M>, stop: StopSignal) {
            loop {
                if stop.is_stopped() {
                    break;
                }
                match self.receiver.recv_timeout(self.poll) {
                    Ok(msg) => {
                        if sender.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
    }

    fn channel_subscription(id: SubId) -> (ChannelSubscription<TestMsg>, mpsc::Sender<TestMsg>) {
        let (tx, rx) = mpsc::channel();
        (ChannelSubscription::new(id, rx), tx)
    }

    #[test]
    fn stop_signal_starts_false() {
        let (signal, _trigger) = StopSignal::new();
        assert!(!signal.is_stopped());
    }

    #[test]
    fn stop_signal_becomes_true_after_trigger() {
        let (signal, trigger) = StopSignal::new();
        trigger.stop();
        assert!(signal.is_stopped());
    }

    #[test]
    fn stop_signal_wait_returns_true_when_stopped() {
        let (signal, trigger) = StopSignal::new();
        trigger.stop();
        assert!(signal.wait_timeout(Duration::from_millis(100)));
    }

    #[test]
    fn stop_signal_wait_returns_false_on_timeout() {
        let (signal, _trigger) = StopSignal::new();
        assert!(!signal.wait_timeout(Duration::from_millis(10)));
    }

    #[test]
    fn channel_subscription_forwards_messages() {
        let (sub, event_tx) = channel_subscription(1);
        let (tx, rx) = mpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        event_tx.send(TestMsg::Value(1)).unwrap();
        event_tx.send(TestMsg::Value(2)).unwrap();
        thread::sleep(Duration::from_millis(10));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<_> = rx.try_iter().collect();
        assert_eq!(msgs, vec![TestMsg::Value(1), TestMsg::Value(2)]);
    }

    #[test]
    fn every_subscription_fires() {
        let sub = Every::new(Duration::from_millis(10), || TestMsg::Tick);
        let (tx, rx) = mpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        // Wait for a few ticks
        thread::sleep(Duration::from_millis(50));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<_> = rx.try_iter().collect();
        assert!(!msgs.is_empty(), "Should have received at least one tick");
        assert!(msgs.iter().all(|m| *m == TestMsg::Tick));
    }

    #[test]
    fn every_subscription_uses_stable_id() {
        let sub1 = Every::<TestMsg>::new(Duration::from_secs(1), || TestMsg::Tick);
        let sub2 = Every::<TestMsg>::new(Duration::from_secs(1), || TestMsg::Tick);
        assert_eq!(sub1.id(), sub2.id());
    }

    #[test]
    fn every_subscription_different_intervals_different_ids() {
        let sub1 = Every::<TestMsg>::new(Duration::from_secs(1), || TestMsg::Tick);
        let sub2 = Every::<TestMsg>::new(Duration::from_secs(2), || TestMsg::Tick);
        assert_ne!(sub1.id(), sub2.id());
    }

    #[test]
    fn subscription_manager_starts_subscriptions() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let (sub, event_tx) = channel_subscription(1);
        let subs: Vec<Box<dyn Subscription<TestMsg>>> = vec![Box::new(sub)];

        mgr.reconcile(subs);
        event_tx.send(TestMsg::Value(42)).unwrap();

        // Give the thread a moment to send
        thread::sleep(Duration::from_millis(20));

        let msgs = mgr.drain_messages();
        assert_eq!(msgs, vec![TestMsg::Value(42)]);
    }

    #[test]
    fn subscription_manager_dedupes_duplicate_ids() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let (sub_a, tx_a) = channel_subscription(7);
        let (sub_b, tx_b) = channel_subscription(7);
        let subs: Vec<Box<dyn Subscription<TestMsg>>> = vec![Box::new(sub_a), Box::new(sub_b)];

        mgr.reconcile(subs);

        tx_a.send(TestMsg::Value(1)).unwrap();
        assert!(
            tx_b.send(TestMsg::Value(2)).is_err(),
            "Duplicate subscription should be dropped"
        );

        thread::sleep(Duration::from_millis(20));
        let msgs = mgr.drain_messages();
        assert_eq!(msgs, vec![TestMsg::Value(1)]);
    }

    #[test]
    fn subscription_manager_stops_removed() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Start with one subscription
        mgr.reconcile(vec![Box::new(Every::with_id(
            99,
            Duration::from_millis(5),
            || TestMsg::Tick,
        ))]);

        thread::sleep(Duration::from_millis(20));
        let msgs_before = mgr.drain_messages();
        assert!(!msgs_before.is_empty());

        // Remove it
        mgr.reconcile(vec![]);

        // Drain any remaining buffered messages
        thread::sleep(Duration::from_millis(20));
        let _ = mgr.drain_messages();

        // After stopping, no more messages should arrive
        thread::sleep(Duration::from_millis(30));
        let msgs_after = mgr.drain_messages();
        assert!(
            msgs_after.is_empty(),
            "Should stop receiving after reconcile with empty set"
        );
    }

    #[test]
    fn subscription_manager_keeps_unchanged() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Start subscription
        mgr.reconcile(vec![Box::new(Every::with_id(
            50,
            Duration::from_millis(10),
            || TestMsg::Tick,
        ))]);

        thread::sleep(Duration::from_millis(30));
        let _ = mgr.drain_messages();

        // Reconcile with same ID - should keep running
        mgr.reconcile(vec![Box::new(Every::with_id(
            50,
            Duration::from_millis(10),
            || TestMsg::Tick,
        ))]);

        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();
        assert!(!msgs.is_empty(), "Subscription should still be running");
    }

    #[test]
    fn subscription_manager_stop_all() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        mgr.reconcile(vec![
            Box::new(Every::with_id(1, Duration::from_millis(5), || {
                TestMsg::Value(1)
            })),
            Box::new(Every::with_id(2, Duration::from_millis(5), || {
                TestMsg::Value(2)
            })),
        ]);

        thread::sleep(Duration::from_millis(20));
        mgr.stop_all();

        thread::sleep(Duration::from_millis(20));
        let _ = mgr.drain_messages();
        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();
        assert!(msgs.is_empty());
    }

    // =========================================================================
    // ADDITIONAL TESTS - Cmd sequencing + Subscriptions (bd-2nu8.10.2)
    // =========================================================================

    #[test]
    fn stop_signal_is_cloneable() {
        let (signal, trigger) = StopSignal::new();
        let signal_clone = signal.clone();

        assert!(!signal.is_stopped());
        assert!(!signal_clone.is_stopped());

        trigger.stop();

        assert!(signal.is_stopped());
        assert!(signal_clone.is_stopped());
    }

    #[test]
    fn stop_signal_wait_wakes_immediately_when_already_stopped() {
        let (signal, trigger) = StopSignal::new();
        trigger.stop();

        // Should return immediately, not wait for timeout
        let start = Instant::now();
        let stopped = signal.wait_timeout(Duration::from_secs(10));
        let elapsed = start.elapsed();

        assert!(stopped);
        assert!(elapsed < Duration::from_millis(100));
    }

    #[test]
    fn stop_signal_wait_is_interrupted_by_trigger() {
        let (signal, trigger) = StopSignal::new();

        let signal_clone = signal.clone();
        let handle = thread::spawn(move || signal_clone.wait_timeout(Duration::from_secs(10)));

        // Give thread time to start waiting
        thread::sleep(Duration::from_millis(20));
        trigger.stop();

        let stopped = handle.join().unwrap();
        assert!(stopped);
    }

    #[test]
    fn channel_subscription_no_messages_without_events() {
        let (sub, _event_tx) = channel_subscription(1);
        let (tx, rx) = mpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(10));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<_> = rx.try_iter().collect();
        assert!(msgs.is_empty());
    }

    #[test]
    fn channel_subscription_id_is_preserved() {
        let (sub, _tx) = channel_subscription(42);
        assert_eq!(sub.id(), 42);
    }

    #[test]
    fn channel_subscription_stops_on_disconnected_receiver() {
        let (sub, event_tx) = channel_subscription(1);
        let (tx, _rx) = mpsc::channel();
        let (signal, _trigger) = StopSignal::new();

        drop(event_tx);

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        let result = handle.join();
        assert!(result.is_ok());
    }

    #[test]
    fn every_with_id_preserves_custom_id() {
        let sub = Every::<TestMsg>::with_id(12345, Duration::from_secs(1), || TestMsg::Tick);
        assert_eq!(sub.id(), 12345);
    }

    #[test]
    fn every_stops_on_disconnected_receiver() {
        let sub = Every::new(Duration::from_millis(5), || TestMsg::Tick);
        let (tx, rx) = mpsc::channel();
        let (signal, _trigger) = StopSignal::new();

        // Drop receiver before running
        drop(rx);

        // Should exit the loop when send fails
        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        // Should complete quickly, not hang
        let result = handle.join();
        assert!(result.is_ok());
    }

    #[test]
    fn every_respects_interval() {
        let sub = Every::with_id(1, Duration::from_millis(50), || TestMsg::Tick);
        let (tx, rx) = mpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let start = Instant::now();
        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        // Wait for 3 ticks worth of time
        thread::sleep(Duration::from_millis(160));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<_> = rx.try_iter().collect();
        let elapsed = start.elapsed();

        // Should have approximately 3 ticks (at 50ms intervals over 160ms)
        assert!(
            msgs.len() >= 2,
            "Expected at least 2 ticks, got {}",
            msgs.len()
        );
        assert!(
            msgs.len() <= 4,
            "Expected at most 4 ticks, got {}",
            msgs.len()
        );
        assert!(elapsed >= Duration::from_millis(150));
    }

    #[test]
    fn subscription_manager_empty_reconcile() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Reconcile with empty list should not panic
        mgr.reconcile(vec![]);
        let msgs = mgr.drain_messages();
        assert!(msgs.is_empty());
    }

    #[test]
    fn subscription_manager_drain_messages_returns_all() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let (sub, event_tx) = channel_subscription(1);
        let subs: Vec<Box<dyn Subscription<TestMsg>>> = vec![Box::new(sub)];

        mgr.reconcile(subs);
        event_tx.send(TestMsg::Value(1)).unwrap();
        event_tx.send(TestMsg::Value(2)).unwrap();
        thread::sleep(Duration::from_millis(20));

        let msgs = mgr.drain_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0], TestMsg::Value(1));
        assert_eq!(msgs[1], TestMsg::Value(2));

        // Second drain should be empty
        let msgs2 = mgr.drain_messages();
        assert!(msgs2.is_empty());
    }

    #[test]
    fn subscription_manager_replaces_subscription_with_different_id() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let (sub1, tx1) = channel_subscription(1);

        // Start with ID 1
        mgr.reconcile(vec![Box::new(sub1)]);
        tx1.send(TestMsg::Value(1)).unwrap();
        thread::sleep(Duration::from_millis(20));
        let msgs1 = mgr.drain_messages();
        assert_eq!(msgs1, vec![TestMsg::Value(1)]);

        // Replace with ID 2
        let (sub2, tx2) = channel_subscription(2);
        mgr.reconcile(vec![Box::new(sub2)]);
        tx2.send(TestMsg::Value(2)).unwrap();
        thread::sleep(Duration::from_millis(20));
        let msgs2 = mgr.drain_messages();
        assert_eq!(msgs2, vec![TestMsg::Value(2)]);
    }

    #[test]
    fn subscription_manager_multiple_subscriptions() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let (sub1, tx1) = channel_subscription(1);
        let (sub2, tx2) = channel_subscription(2);
        let (sub3, tx3) = channel_subscription(3);
        let subs: Vec<Box<dyn Subscription<TestMsg>>> =
            vec![Box::new(sub1), Box::new(sub2), Box::new(sub3)];

        mgr.reconcile(subs);
        tx1.send(TestMsg::Value(10)).unwrap();
        tx2.send(TestMsg::Value(20)).unwrap();
        tx3.send(TestMsg::Value(30)).unwrap();
        thread::sleep(Duration::from_millis(30));

        let mut msgs = mgr.drain_messages();
        msgs.sort_by_key(|m| match m {
            TestMsg::Value(v) => *v,
            _ => 0,
        });

        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0], TestMsg::Value(10));
        assert_eq!(msgs[1], TestMsg::Value(20));
        assert_eq!(msgs[2], TestMsg::Value(30));
    }

    #[test]
    fn subscription_manager_partial_update() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Start with 3 subscriptions
        mgr.reconcile(vec![
            Box::new(Every::with_id(1, Duration::from_millis(10), || {
                TestMsg::Value(1)
            })),
            Box::new(Every::with_id(2, Duration::from_millis(10), || {
                TestMsg::Value(2)
            })),
            Box::new(Every::with_id(3, Duration::from_millis(10), || {
                TestMsg::Value(3)
            })),
        ]);

        thread::sleep(Duration::from_millis(30));
        let _ = mgr.drain_messages();

        // Remove subscription 2, keep 1 and 3
        mgr.reconcile(vec![
            Box::new(Every::with_id(1, Duration::from_millis(10), || {
                TestMsg::Value(1)
            })),
            Box::new(Every::with_id(3, Duration::from_millis(10), || {
                TestMsg::Value(3)
            })),
        ]);

        // Drain any in-flight messages that were sent before the stop signal was processed.
        // This clears the race window between stop signal and message send.
        let _ = mgr.drain_messages();

        // Now wait for new messages from the remaining subscriptions
        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();

        // Should only have values 1 and 3, not 2
        let values: Vec<i32> = msgs
            .iter()
            .filter_map(|m| match m {
                TestMsg::Value(v) => Some(*v),
                _ => None,
            })
            .collect();

        assert!(
            values.contains(&1),
            "Should still receive from subscription 1"
        );
        assert!(
            values.contains(&3),
            "Should still receive from subscription 3"
        );
        assert!(
            !values.contains(&2),
            "Should not receive from stopped subscription 2"
        );
    }

    #[test]
    fn subscription_manager_drop_stops_all() {
        let (_signal, _) = StopSignal::new();
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag_clone = flag.clone();

        struct FlagSubscription {
            id: SubId,
            flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }

        impl Subscription<TestMsg> for FlagSubscription {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(5));
                }
                self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        {
            let mut mgr = SubscriptionManager::<TestMsg>::new();
            mgr.reconcile(vec![Box::new(FlagSubscription {
                id: 1,
                flag: flag_clone,
            })]);

            thread::sleep(Duration::from_millis(20));
            // mgr drops here, should stop all subscriptions
        }

        thread::sleep(Duration::from_millis(50));
        assert!(
            flag.load(std::sync::atomic::Ordering::SeqCst),
            "Subscription should have stopped on drop"
        );
    }

    #[test]
    fn running_subscription_stop_joins_thread() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let completed = std::sync::Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();

        let (signal, trigger) = StopSignal::new();
        let (_tx, _rx) = mpsc::channel::<TestMsg>();

        let thread = thread::spawn(move || {
            while !signal.is_stopped() {
                thread::sleep(Duration::from_millis(5));
            }
            completed_clone.store(true, Ordering::SeqCst);
        });

        let running = RunningSubscription {
            id: 1,
            trigger,
            thread: Some(thread),
            panicked: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        running.stop();
        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn running_subscription_stop_times_out_for_uncooperative_thread() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let completed = std::sync::Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();

        let (_signal, trigger) = StopSignal::new();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            completed_clone.store(true, Ordering::SeqCst);
        });

        let running = RunningSubscription {
            id: 7,
            trigger,
            thread: Some(thread),
            panicked: std::sync::Arc::new(AtomicBool::new(false)),
        };

        let start = Instant::now();
        running.stop();
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "stop() should not block behind an uncooperative subscription thread"
        );

        thread::sleep(Duration::from_millis(550));
        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn every_id_stable_across_instances() {
        // Same interval should produce same ID
        let sub1 = Every::<TestMsg>::new(Duration::from_millis(100), || TestMsg::Tick);
        let sub2 = Every::<TestMsg>::new(Duration::from_millis(100), || TestMsg::Tick);
        let sub3 = Every::<TestMsg>::new(Duration::from_millis(100), || TestMsg::Value(1));

        assert_eq!(sub1.id(), sub2.id());
        assert_eq!(sub2.id(), sub3.id()); // ID is based on interval, not message factory
    }

    #[test]
    fn drain_messages_preserves_order() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Use a custom subscription that sends messages in order
        struct OrderedSubscription {
            values: Vec<i32>,
        }

        impl Subscription<TestMsg> for OrderedSubscription {
            fn id(&self) -> SubId {
                999
            }

            fn run(&self, sender: mpsc::Sender<TestMsg>, _stop: StopSignal) {
                for v in &self.values {
                    let _ = sender.send(TestMsg::Value(*v));
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }

        mgr.reconcile(vec![Box::new(OrderedSubscription {
            values: vec![1, 2, 3, 4, 5],
        })]);

        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();

        let values: Vec<i32> = msgs
            .iter()
            .filter_map(|m| match m {
                TestMsg::Value(v) => Some(*v),
                _ => None,
            })
            .collect();

        assert_eq!(values, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn subscription_manager_new_is_empty() {
        let mgr = SubscriptionManager::<TestMsg>::new();
        let msgs = mgr.drain_messages();
        assert!(msgs.is_empty());
    }

    // =========================================================================
    // LIFECYCLE CONTRACT TESTS (bd-1dg21)
    //
    // These tests capture the observable behavioral contract of the subscription
    // system that MUST be preserved during the Asupersync migration. Each test
    // documents a specific guarantee that downstream code relies on.
    // =========================================================================

    /// CONTRACT: StopSignal backed by CancellationToken must remain functional
    /// even after concurrent thread panics. The AtomicBool-based implementation
    /// is inherently poison-resistant.
    #[test]
    fn contract_stop_signal_resilient_to_thread_panics() {
        let (signal, trigger) = StopSignal::new();
        let signal_clone = signal.clone();

        // Panic in a thread that holds a clone of the signal
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert!(!signal_clone.is_stopped());
            panic!("intentional panic while holding signal clone");
        }));
        assert!(result.is_err());

        // Signal must still be checkable and triggerable after thread panic
        assert!(
            !signal.is_stopped(),
            "signal should still report not-stopped"
        );
        trigger.stop();
        assert!(
            signal.is_stopped(),
            "signal should report stopped after trigger"
        );
        assert!(
            signal.wait_timeout(Duration::from_millis(10)),
            "wait_timeout should return true when stopped"
        );
    }

    /// CONTRACT: StopSignal exposes its underlying CancellationToken for
    /// Asupersync integration.
    #[test]
    fn contract_stop_signal_exposes_cancellation_token() {
        let (signal, trigger) = StopSignal::new();
        let token = signal.cancellation_token();
        assert!(!token.is_cancelled(), "token should start uncancelled");
        trigger.stop();
        assert!(token.is_cancelled(), "token should be cancelled after stop");
    }

    /// CONTRACT: stop_all() must complete within a bounded time even if subscription
    /// threads are uncooperative. The 250ms join timeout per subscription is the
    /// upper bound.
    #[test]
    fn contract_stop_all_bounded_time_with_uncooperative_subscriptions() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Create subscriptions that ignore the stop signal
        struct UncooperativeSub {
            id: SubId,
        }

        impl Subscription<TestMsg> for UncooperativeSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, _stop: StopSignal) {
                // Ignore stop signal entirely, sleep for a long time
                thread::sleep(Duration::from_secs(5));
            }
        }

        mgr.reconcile(vec![
            Box::new(UncooperativeSub { id: 100 }),
            Box::new(UncooperativeSub { id: 200 }),
        ]);

        thread::sleep(Duration::from_millis(20)); // let threads start

        let start = Instant::now();
        mgr.stop_all();
        let elapsed = start.elapsed();

        // 2 subscriptions * 250ms timeout each = 500ms max, plus some margin
        assert!(
            elapsed < Duration::from_millis(800),
            "stop_all took {elapsed:?}, expected < 800ms for 2 uncooperative subscriptions"
        );
    }

    /// CONTRACT: reconcile() must not start a new subscription for an ID that is
    /// already active, even if the subscription object is different.
    #[test]
    fn contract_reconcile_deduplicates_by_id_not_identity() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        struct CountingSub {
            id: SubId,
            counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl Subscription<TestMsg> for CountingSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                self.counter
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }

        // First reconcile starts one thread
        mgr.reconcile(vec![Box::new(CountingSub {
            id: 42,
            counter: counter.clone(),
        })]);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "first reconcile should start exactly 1 thread"
        );

        // Second reconcile with same ID must NOT start another thread
        mgr.reconcile(vec![Box::new(CountingSub {
            id: 42,
            counter: counter.clone(),
        })]);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "second reconcile with same ID must not start another thread"
        );

        mgr.stop_all();
    }

    /// CONTRACT: When a subscription is removed via reconcile(), messages it sent
    /// before being stopped may still be in the channel. drain_messages() must
    /// return these buffered messages.
    #[test]
    fn contract_buffered_messages_available_after_subscription_stopped() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        struct BurstSub;

        impl Subscription<TestMsg> for BurstSub {
            fn id(&self) -> SubId {
                77
            }

            fn run(&self, sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                // Send a burst of messages immediately
                for i in 0..10 {
                    let _ = sender.send(TestMsg::Value(i));
                }
                // Then wait for stop
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }

        mgr.reconcile(vec![Box::new(BurstSub)]);
        thread::sleep(Duration::from_millis(30));

        // Remove the subscription
        mgr.reconcile(vec![]);

        // Messages sent before stop should still be drainable
        let msgs = mgr.drain_messages();
        let values: Vec<i32> = msgs
            .iter()
            .filter_map(|m| match m {
                TestMsg::Value(v) => Some(*v),
                _ => None,
            })
            .collect();

        assert!(
            values.len() >= 5,
            "Expected at least 5 buffered messages after stop, got {}",
            values.len()
        );
    }

    /// CONTRACT: active_count() must accurately reflect the number of running
    /// subscriptions at all times.
    #[test]
    fn contract_active_count_tracks_running_subscriptions() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        assert_eq!(mgr.active_count(), 0, "empty manager");

        mgr.reconcile(vec![
            Box::new(Every::with_id(1, Duration::from_millis(50), || {
                TestMsg::Tick
            })),
            Box::new(Every::with_id(2, Duration::from_millis(50), || {
                TestMsg::Tick
            })),
        ]);
        assert_eq!(mgr.active_count(), 2, "after starting 2");

        mgr.reconcile(vec![Box::new(Every::with_id(
            1,
            Duration::from_millis(50),
            || TestMsg::Tick,
        ))]);
        assert_eq!(mgr.active_count(), 1, "after removing 1");

        mgr.stop_all();
        assert_eq!(mgr.active_count(), 0, "after stop_all");
    }

    /// CONTRACT: The Every subscription ID must be derived from interval only,
    /// not from the message factory closure. Two Every subscriptions with the
    /// same interval MUST have the same ID regardless of message content.
    #[test]
    fn contract_every_id_derived_from_interval_only() {
        let sub_a = Every::<TestMsg>::new(Duration::from_millis(100), || TestMsg::Tick);
        let sub_b = Every::<TestMsg>::new(Duration::from_millis(100), || TestMsg::Value(999));
        assert_eq!(
            sub_a.id(),
            sub_b.id(),
            "Every ID must depend only on interval, not message factory"
        );

        let sub_c = Every::<TestMsg>::new(Duration::from_millis(200), || TestMsg::Tick);
        assert_ne!(
            sub_a.id(),
            sub_c.id(),
            "Different intervals must produce different IDs"
        );
    }

    /// CONTRACT: The Every subscription ID formula must remain stable across
    /// versions. This captures the exact formula: interval_nanos XOR 0x5449_434B.
    #[test]
    fn contract_every_id_formula_is_stable() {
        let interval = Duration::from_millis(100);
        let expected_id = interval.as_nanos() as u64 ^ 0x5449_434B;
        let sub = Every::<TestMsg>::new(interval, || TestMsg::Tick);
        assert_eq!(
            sub.id(),
            expected_id,
            "Every ID formula must be: interval.as_nanos() as u64 ^ 0x5449_434B"
        );
    }

    /// CONTRACT: Drop on SubscriptionManager must stop all subscriptions.
    /// This is the safety net for cleanup even if stop_all() is not called.
    #[test]
    fn contract_drop_triggers_stop_all() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let stop_count = std::sync::Arc::new(AtomicUsize::new(0));

        struct StopCountingSub {
            id: SubId,
            counter: std::sync::Arc<AtomicUsize>,
        }

        impl Subscription<TestMsg> for StopCountingSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(5));
                }
                self.counter.fetch_add(1, Ordering::SeqCst);
            }
        }

        {
            let mut mgr = SubscriptionManager::<TestMsg>::new();
            mgr.reconcile(vec![
                Box::new(StopCountingSub {
                    id: 1,
                    counter: stop_count.clone(),
                }),
                Box::new(StopCountingSub {
                    id: 2,
                    counter: stop_count.clone(),
                }),
                Box::new(StopCountingSub {
                    id: 3,
                    counter: stop_count.clone(),
                }),
            ]);
            thread::sleep(Duration::from_millis(20));
            // mgr dropped here
        }

        // Give threads time to notice stop signal and exit
        thread::sleep(Duration::from_millis(400));
        assert_eq!(
            stop_count.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "all 3 subscription threads must have observed stop signal on drop"
        );
    }

    /// CONTRACT: SUBSCRIPTION_STOP_JOIN_TIMEOUT must be exactly 250ms.
    /// The Asupersync migration must preserve this timeout bound.
    #[test]
    fn contract_stop_join_timeout_is_250ms() {
        assert_eq!(
            SUBSCRIPTION_STOP_JOIN_TIMEOUT,
            Duration::from_millis(250),
            "join timeout must be 250ms"
        );
        assert_eq!(
            SUBSCRIPTION_STOP_JOIN_POLL,
            Duration::from_millis(1),
            "join poll interval must be 1ms (bd-1f2aw)"
        );
    }

    // =========================================================================
    // STRUCTURED LIFECYCLE TESTS (bd-1f2aw)
    //
    // These tests validate the structured cancellation, panic resilience,
    // and parallel shutdown improvements.
    // =========================================================================

    /// bd-1f2aw: A panicking subscription must not crash the runtime.
    /// The panic is caught, the panicked flag is set, and telemetry is emitted.
    #[test]
    fn lifecycle_panic_in_subscription_is_caught() {
        use std::sync::atomic::Ordering;

        let mut mgr = SubscriptionManager::<TestMsg>::new();

        struct PanickingSub;

        impl Subscription<TestMsg> for PanickingSub {
            fn id(&self) -> SubId {
                0xDEAD
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, _stop: StopSignal) {
                panic!("intentional test panic in subscription");
            }
        }

        mgr.reconcile(vec![Box::new(PanickingSub)]);

        // Give the thread time to panic and be caught.
        thread::sleep(Duration::from_millis(50));

        // The manager should still be functional.
        assert_eq!(
            mgr.active_count(),
            1,
            "panicked sub still tracked as active"
        );

        // The panicked flag should be set.
        assert!(
            mgr.active[0].panicked.load(Ordering::Acquire),
            "panicked flag should be set after subscription panic"
        );

        // stop_all should not panic even with a panicked subscription.
        mgr.stop_all();
        assert_eq!(mgr.active_count(), 0);
    }

    /// bd-1f2aw: A panicking subscription must not prevent other subscriptions
    /// from continuing to deliver messages.
    #[test]
    fn lifecycle_panic_does_not_affect_sibling_subscriptions() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        struct PanickingSub;
        impl Subscription<TestMsg> for PanickingSub {
            fn id(&self) -> SubId {
                0xBAD
            }
            fn run(&self, _sender: mpsc::Sender<TestMsg>, _stop: StopSignal) {
                panic!("boom");
            }
        }

        mgr.reconcile(vec![
            Box::new(PanickingSub),
            Box::new(Every::with_id(42, Duration::from_millis(10), || {
                TestMsg::Tick
            })),
        ]);

        // Wait for panic to happen and ticks to arrive.
        // The tick subscription fires every 10ms; wait long enough for several.
        thread::sleep(Duration::from_millis(100));

        let msgs = mgr.drain_messages();
        assert!(
            !msgs.is_empty(),
            "sibling subscription should still deliver messages after a panic in another sub"
        );

        mgr.stop_all();
    }

    /// bd-1f2aw: Parallel phased shutdown (stop_all) must be faster than
    /// sequential shutdown when multiple subscriptions need to wind down.
    #[test]
    fn lifecycle_stop_all_parallel_shutdown() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let stop_count = std::sync::Arc::new(AtomicUsize::new(0));
        let sub_count = 4;

        struct SlowStopSub {
            id: SubId,
            counter: std::sync::Arc<AtomicUsize>,
        }

        impl Subscription<TestMsg> for SlowStopSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                // Wait for stop, then simulate slow cleanup (50ms).
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(5));
                }
                thread::sleep(Duration::from_millis(50));
                self.counter.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut mgr = SubscriptionManager::<TestMsg>::new();
        let subs: Vec<Box<dyn Subscription<TestMsg>>> = (0..sub_count)
            .map(|i| -> Box<dyn Subscription<TestMsg>> {
                Box::new(SlowStopSub {
                    id: 1000 + i,
                    counter: stop_count.clone(),
                })
            })
            .collect();

        mgr.reconcile(subs);
        thread::sleep(Duration::from_millis(20));

        let start = Instant::now();
        mgr.stop_all();
        let elapsed = start.elapsed();

        // With parallel signal, all 4 subs start their 50ms cleanup
        // simultaneously. Sequential would take ~200ms (4 * 50ms).
        // Parallel should complete in ~50ms + join overhead.
        // Use 150ms as a generous bound (well under 200ms sequential).
        assert!(
            elapsed < Duration::from_millis(150),
            "parallel stop_all took {elapsed:?}, expected < 150ms \
             (sequential would be ~{expected_sequential}ms)",
            expected_sequential = sub_count * 50
        );

        // All subscriptions should have completed cleanup.
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            stop_count.load(Ordering::SeqCst),
            sub_count as usize,
            "all subscriptions should have completed their cleanup"
        );
    }

    /// bd-1f2aw: Two-phase signal+join in reconcile should allow parallel
    /// wind-down when removing multiple subscriptions at once.
    #[test]
    fn lifecycle_reconcile_removal_uses_parallel_stop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let stop_count = std::sync::Arc::new(AtomicUsize::new(0));

        struct SlowStopSub {
            id: SubId,
            counter: std::sync::Arc<AtomicUsize>,
        }

        impl Subscription<TestMsg> for SlowStopSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(5));
                }
                thread::sleep(Duration::from_millis(40));
                self.counter.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut mgr = SubscriptionManager::<TestMsg>::new();
        mgr.reconcile(vec![
            Box::new(SlowStopSub {
                id: 2000,
                counter: stop_count.clone(),
            }),
            Box::new(SlowStopSub {
                id: 2001,
                counter: stop_count.clone(),
            }),
            Box::new(SlowStopSub {
                id: 2002,
                counter: stop_count.clone(),
            }),
        ]);
        thread::sleep(Duration::from_millis(20));

        // Remove all subscriptions via reconcile.
        let start = Instant::now();
        mgr.reconcile(vec![]);
        let elapsed = start.elapsed();

        // Parallel: ~40ms + overhead. Sequential would be ~120ms.
        assert!(
            elapsed < Duration::from_millis(100),
            "reconcile removal took {elapsed:?}, expected < 100ms with parallel stop"
        );

        thread::sleep(Duration::from_millis(20));
        assert_eq!(stop_count.load(Ordering::SeqCst), 3);
    }

    /// bd-1f2aw: has_panicked() must reflect the actual panic state of the thread.
    #[test]
    fn lifecycle_has_panicked_tracks_state() {
        let panicked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let panicked_flag = panicked.clone();

        let (signal, trigger) = StopSignal::new();
        let thread = thread::spawn(move || {
            signal.wait_timeout(Duration::from_secs(10));
        });

        let running = RunningSubscription {
            id: 999,
            trigger,
            thread: Some(thread),
            panicked,
        };

        assert!(!running.has_panicked(), "should not be panicked initially");

        // Simulate a panic flag (normally set by the catch_unwind wrapper).
        panicked_flag.store(true, std::sync::atomic::Ordering::Release);
        assert!(running.has_panicked(), "should reflect panicked state");

        running.stop();
    }

    /// bd-1f2aw: signal_stop + join_bounded should work correctly as a two-phase
    /// shutdown for individual subscriptions.
    #[test]
    fn lifecycle_signal_then_join_works() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let completed = std::sync::Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();

        let (signal, trigger) = StopSignal::new();
        let thread = thread::spawn(move || {
            while !signal.is_stopped() {
                thread::sleep(Duration::from_millis(5));
            }
            completed_clone.store(true, Ordering::SeqCst);
        });

        let running = RunningSubscription {
            id: 888,
            trigger,
            thread: Some(thread),
            panicked: std::sync::Arc::new(AtomicBool::new(false)),
        };

        running.signal_stop();
        let leftover = running.join_bounded();
        assert!(
            leftover.is_none(),
            "cooperative thread should join within timeout"
        );
        assert!(
            completed.load(Ordering::SeqCst),
            "thread should have completed"
        );
    }

    /// bd-1f2aw: join_bounded must return the handle for uncooperative threads.
    #[test]
    fn lifecycle_join_bounded_returns_handle_for_uncooperative() {
        use std::sync::atomic::AtomicBool;

        let (_signal, trigger) = StopSignal::new();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
        });

        let running = RunningSubscription {
            id: 777,
            trigger,
            thread: Some(thread),
            panicked: std::sync::Arc::new(AtomicBool::new(false)),
        };

        running.signal_stop();
        let start = Instant::now();
        let leftover = running.join_bounded();
        let elapsed = start.elapsed();

        assert!(
            leftover.is_some(),
            "uncooperative thread should not join within timeout"
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "join_bounded should respect the 250ms timeout, took {elapsed:?}"
        );
    }

    /// bd-1f2aw: Restart semantics — a subscription that was stopped via
    /// reconcile can be re-started by including it in a subsequent reconcile.
    #[test]
    fn lifecycle_restart_after_stop() {
        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Start subscription.
        mgr.reconcile(vec![Box::new(Every::with_id(
            300,
            Duration::from_millis(10),
            || TestMsg::Tick,
        ))]);
        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();
        assert!(!msgs.is_empty(), "should receive ticks");

        // Remove it.
        mgr.reconcile(vec![]);
        thread::sleep(Duration::from_millis(20));
        let _ = mgr.drain_messages();
        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();
        assert!(msgs.is_empty(), "should stop receiving after removal");

        // Restart with same ID.
        mgr.reconcile(vec![Box::new(Every::with_id(
            300,
            Duration::from_millis(10),
            || TestMsg::Value(99),
        ))]);
        thread::sleep(Duration::from_millis(30));
        let msgs = mgr.drain_messages();
        assert!(
            !msgs.is_empty(),
            "should receive messages again after restart"
        );
        assert!(
            msgs.iter().any(|m| matches!(m, TestMsg::Value(99))),
            "restarted sub should use the new message factory"
        );

        mgr.stop_all();
    }

    /// bd-1f2aw: Non-interference contract — subscriptions communicate
    /// exclusively through the mpsc channel. They have no access to terminal
    /// state, frame buffers, or render surfaces.
    ///
    /// This test verifies the architectural invariant by demonstrating that
    /// subscription threads only interact with the manager through messages,
    /// and that the manager's state is consistent after concurrent operations.
    #[test]
    fn lifecycle_non_interference_with_manager_state() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let msg_count = std::sync::Arc::new(AtomicUsize::new(0));

        struct CountingSub {
            id: SubId,
            counter: std::sync::Arc<AtomicUsize>,
        }

        impl Subscription<TestMsg> for CountingSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                while !stop.is_stopped() {
                    if sender.send(TestMsg::Tick).is_err() {
                        break;
                    }
                    self.counter.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }

        let mut mgr = SubscriptionManager::<TestMsg>::new();

        // Start multiple subscriptions.
        mgr.reconcile(vec![
            Box::new(CountingSub {
                id: 400,
                counter: msg_count.clone(),
            }),
            Box::new(CountingSub {
                id: 401,
                counter: msg_count.clone(),
            }),
        ]);

        thread::sleep(Duration::from_millis(50));

        // Manager state is consistent while subscriptions are running.
        assert_eq!(mgr.active_count(), 2);
        let drained = mgr.drain_messages();
        let sent_count = msg_count.load(Ordering::SeqCst);
        assert!(sent_count > 0, "subscriptions should have sent messages");
        assert!(
            drained.len() <= sent_count,
            "drained {} but only {} sent",
            drained.len(),
            sent_count
        );

        // Stop all — manager state is consistent after shutdown.
        mgr.stop_all();
        assert_eq!(mgr.active_count(), 0);

        // Drain remaining buffered messages.
        let remaining = mgr.drain_messages();
        let total_drained = drained.len() + remaining.len();
        let final_sent = msg_count.load(Ordering::SeqCst);
        assert!(
            total_drained <= final_sent,
            "total drained ({total_drained}) must not exceed total sent ({final_sent})"
        );
    }

    /// bd-1f2aw: Shutdown ordering contract — stop_all() must signal all
    /// subscriptions before joining any. Verify by checking that all subs
    /// observe the stop signal approximately simultaneously.
    #[test]
    fn lifecycle_shutdown_signal_ordering() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let signal_times =
            std::sync::Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
        let epoch = Instant::now();

        struct TimingStopSub {
            id: SubId,
            index: usize,
            signal_times: std::sync::Arc<[AtomicU64; 3]>,
            epoch: Instant,
        }

        impl Subscription<TestMsg> for TimingStopSub {
            fn id(&self) -> SubId {
                self.id
            }

            fn run(&self, _sender: mpsc::Sender<TestMsg>, stop: StopSignal) {
                while !stop.is_stopped() {
                    thread::sleep(Duration::from_millis(1));
                }
                let elapsed_us = self.epoch.elapsed().as_micros() as u64;
                self.signal_times[self.index].store(elapsed_us, Ordering::SeqCst);
            }
        }

        let mut mgr = SubscriptionManager::<TestMsg>::new();
        mgr.reconcile(vec![
            Box::new(TimingStopSub {
                id: 500,
                index: 0,
                signal_times: signal_times.clone(),
                epoch,
            }),
            Box::new(TimingStopSub {
                id: 501,
                index: 1,
                signal_times: signal_times.clone(),
                epoch,
            }),
            Box::new(TimingStopSub {
                id: 502,
                index: 2,
                signal_times: signal_times.clone(),
                epoch,
            }),
        ]);
        thread::sleep(Duration::from_millis(20));

        mgr.stop_all();

        // All three should have observed the stop signal at approximately
        // the same time (within 10ms of each other), because phase 1 signals
        // all before phase 2 joins any.
        let t0 = signal_times[0].load(Ordering::SeqCst);
        let t1 = signal_times[1].load(Ordering::SeqCst);
        let t2 = signal_times[2].load(Ordering::SeqCst);

        assert!(
            t0 > 0 && t1 > 0 && t2 > 0,
            "all subs should have recorded stop time"
        );

        let max_t = t0.max(t1).max(t2);
        let min_t = t0.min(t1).min(t2);
        let spread_us = max_t - min_t;

        assert!(
            spread_us < 10_000, // 10ms
            "stop signal spread should be < 10ms for parallel signaling, got {spread_us}us \
             (t0={t0}, t1={t1}, t2={t2})"
        );
    }
}
