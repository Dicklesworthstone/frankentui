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

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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
#[derive(Clone)]
pub struct StopSignal {
    inner: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl StopSignal {
    /// Create a new stop signal pair (signal, trigger).
    pub(crate) fn new() -> (Self, StopTrigger) {
        let inner = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let signal = Self {
            inner: inner.clone(),
        };
        let trigger = StopTrigger { inner };
        (signal, trigger)
    }

    /// Check if the stop signal has been triggered.
    pub fn is_stopped(&self) -> bool {
        let (lock, _) = &*self.inner;
        *lock.lock().unwrap()
    }

    /// Wait for either the stop signal or a timeout.
    ///
    /// Returns `true` if stopped, `false` if timed out.
    /// Blocks the thread efficiently using a condition variable.
    pub fn wait_timeout(&self, duration: Duration) -> bool {
        let (lock, cvar) = &*self.inner;
        let mut stopped = lock.lock().unwrap();
        if *stopped {
            return true;
        }
        let result = cvar.wait_timeout(stopped, duration).unwrap();
        stopped = result.0;
        *stopped
    }
}

/// Trigger to stop a subscription from the runtime side.
pub(crate) struct StopTrigger {
    inner: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl StopTrigger {
    /// Signal the subscription to stop.
    pub(crate) fn stop(&self) {
        let (lock, cvar) = &*self.inner;
        let mut stopped = lock.lock().unwrap();
        *stopped = true;
        cvar.notify_all();
    }
}

/// A running subscription handle.
pub(crate) struct RunningSubscription {
    pub(crate) id: SubId,
    trigger: StopTrigger,
    thread: Option<thread::JoinHandle<()>>,
}

impl RunningSubscription {
    /// Stop the subscription and join its thread.
    pub(crate) fn stop(mut self) {
        self.trigger.stop();
        if let Some(handle) = self.thread.take() {
            // Give the thread a moment to finish, but don't block forever
            let _ = handle.join();
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
}

impl<M: Send + 'static> SubscriptionManager<M> {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            active: Vec::new(),
            sender,
            receiver,
        }
    }

    /// Update the set of active subscriptions.
    ///
    /// Compares the new set against currently running subscriptions:
    /// - Starts subscriptions that are new (ID not in active set)
    /// - Stops subscriptions that are no longer declared (ID not in new set)
    /// - Leaves unchanged subscriptions running
    pub(crate) fn reconcile(&mut self, subscriptions: Vec<Box<dyn Subscription<M>>>) {
        let new_ids: Vec<SubId> = subscriptions.iter().map(|s| s.id()).collect();

        // Stop subscriptions that are no longer active
        let mut remaining = Vec::new();
        for running in self.active.drain(..) {
            if new_ids.contains(&running.id) {
                remaining.push(running);
            } else {
                tracing::debug!(sub_id = running.id, "Stopping subscription");
                running.stop();
            }
        }
        self.active = remaining;

        // Start new subscriptions
        let active_ids: Vec<SubId> = self.active.iter().map(|r| r.id).collect();
        for sub in subscriptions {
            let id = sub.id();
            if active_ids.contains(&id) {
                continue;
            }

            tracing::debug!(sub_id = id, "Starting subscription");
            let (signal, trigger) = StopSignal::new();
            let sender = self.sender.clone();

            let thread = thread::spawn(move || {
                sub.run(sender, signal);
            });

            self.active.push(RunningSubscription {
                id,
                trigger,
                thread: Some(thread),
            });
        }
    }

    /// Drain pending messages from subscriptions.
    pub(crate) fn drain_messages(&self) -> Vec<M> {
        let mut messages = Vec::new();
        while let Ok(msg) = self.receiver.try_recv() {
            messages.push(msg);
        }
        messages
    }

    /// Stop all running subscriptions.
    pub(crate) fn stop_all(&mut self) {
        for running in self.active.drain(..) {
            running.stop();
        }
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
        loop {
            if stop.wait_timeout(self.interval) {
                break;
            }
            let msg = (self.make_msg)();
            if sender.send(msg).is_err() {
                break;
            }
        }
    }
}

/// A mock subscription for testing.
///
/// Immediately sends all queued messages and then stops.
pub struct MockSubscription<M: Send + 'static> {
    id: SubId,
    messages: Vec<M>,
}

impl<M: Send + Clone + 'static> MockSubscription<M> {
    /// Create a mock subscription that sends the given messages.
    pub fn new(id: SubId, messages: Vec<M>) -> Self {
        Self { id, messages }
    }
}

impl<M: Send + Clone + 'static> Subscription<M> for MockSubscription<M> {
    fn id(&self) -> SubId {
        self.id
    }

    fn run(&self, sender: mpsc::Sender<M>, _stop: StopSignal) {
        for msg in &self.messages {
            if sender.send(msg.clone()).is_err() {
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
    fn mock_subscription_sends_messages() {
        let sub = MockSubscription::new(1, vec![TestMsg::Value(1), TestMsg::Value(2)]);
        let (tx, rx) = mpsc::channel();
        let (signal, _trigger) = StopSignal::new();

        sub.run(tx, signal);

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
        let subs: Vec<Box<dyn Subscription<TestMsg>>> =
            vec![Box::new(MockSubscription::new(1, vec![TestMsg::Value(42)]))];

        mgr.reconcile(subs);

        // Give the thread a moment to send
        thread::sleep(Duration::from_millis(20));

        let msgs = mgr.drain_messages();
        assert_eq!(msgs, vec![TestMsg::Value(42)]);
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
}
