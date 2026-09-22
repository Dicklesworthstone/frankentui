// SPDX-License-Identifier: Apache-2.0
//! Retry policies and timeout-enforced task helpers.
//!
//! Provides [`RetryPolicy`] for configurable retry-with-backoff and
//! [`task_with_timeout`] / [`task_with_retry`] constructors that wrap
//! [`Cmd::Task`](crate::Cmd) with deterministic lifecycle guarantees.
//!
//! # Migration rationale
//!
//! Source frameworks often have retry/timeout baked into effect middleware.
//! These helpers give the migration code emitter explicit, testable primitives
//! to target instead of ad-hoc retry loops.
//!
//! # Determinism
//!
//! Backoff delays use fixed formulas (no jitter/randomness) so that
//! replay-based determinism tests can reproduce exact timing sequences.
//!
//! # Example
//!
//! ```
//! use ftui_runtime::retry::{RetryPolicy, BackoffStrategy};
//! use std::time::Duration;
//!
//! let policy = RetryPolicy::new(3, BackoffStrategy::Exponential {
//!     base_ms: 100,
//!     max_ms: 5000,
//! });
//!
//! assert_eq!(policy.delay(0), Duration::from_millis(100));
//! assert_eq!(policy.delay(1), Duration::from_millis(200));
//! assert_eq!(policy.delay(2), Duration::from_millis(400));
//! ```

#![forbid(unsafe_code)]

use std::sync::mpsc;

use crate::cancellation::{CancellationSource, CancellationToken};
use crate::program::{Cmd, TaskSpec};
use web_time::Duration;

const TASK_THREAD_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
const TASK_THREAD_JOIN_POLL: Duration = Duration::from_millis(5);

fn duration_from_millis_saturating(millis: u128) -> Duration {
    if millis >= Duration::MAX.as_millis() {
        Duration::MAX
    } else {
        let seconds = millis / 1_000;
        let subsecond_millis = millis % 1_000;
        let Ok(seconds) = u64::try_from(seconds) else {
            return Duration::MAX;
        };
        let Ok(nanos) = u32::try_from(subsecond_millis.saturating_mul(1_000_000)) else {
            return Duration::MAX;
        };
        Duration::new(seconds, nanos)
    }
}

fn add_millis_saturating(total: &mut u128, millis: u128) {
    *total = total.saturating_add(millis).min(Duration::MAX.as_millis());
}

fn join_task_thread(handle: std::thread::JoinHandle<()>) {
    let _ = handle.join();
}

/// Run `f` on a worker thread; `rx` delivers its result or its panic.
///
/// The worker is where the caller's code runs, so it gets what the executor
/// gives a plain task: the panic is caught, with terminal cleanup hooks
/// suppressed. The executor's suppression is per thread and does not reach
/// this one. A panicking worker used to fire the terminal teardown mid-run,
/// then read as a timeout, because its dropped sender looked like one to
/// `recv_timeout`.
fn spawn_worker<T, F>(
    f: F,
) -> (
    std::thread::JoinHandle<()>,
    mpsc::Receiver<std::thread::Result<T>>,
)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = ftui_core::with_panic_cleanup_suppressed(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        });
        let _ = tx.send(result);
    });
    (handle, rx)
}

fn join_task_thread_bounded(handle: std::thread::JoinHandle<()>, task_name: &'static str) {
    let start = web_time::Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= TASK_THREAD_JOIN_TIMEOUT {
            tracing::warn!(
                task = task_name,
                timeout_ms = TASK_THREAD_JOIN_TIMEOUT.as_millis() as u64,
                "Timed-out worker thread did not exit within the cancellation join timeout; detaching"
            );
            return;
        }
        std::thread::sleep(TASK_THREAD_JOIN_POLL);
    }
    join_task_thread(handle);
}

/// Backoff strategy for retry delays.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub enum BackoffStrategy {
    /// Fixed delay between retries.
    Fixed {
        /// Delay in milliseconds.
        delay_ms: u64,
    },
    /// Exponential backoff: `base_ms * 2^attempt`, capped at `max_ms`.
    Exponential {
        /// Base delay in milliseconds.
        base_ms: u64,
        /// Maximum delay cap in milliseconds.
        max_ms: u64,
    },
    /// Linear backoff: `base_ms * (attempt + 1)`, capped at `max_ms`.
    Linear {
        /// Base delay in milliseconds.
        base_ms: u64,
        /// Maximum delay cap in milliseconds.
        max_ms: u64,
    },
}

/// A retry policy with configurable attempts and backoff.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retries, just the initial attempt).
    pub max_retries: u32,
    /// Backoff strategy between retries.
    pub backoff: BackoffStrategy,
}

impl RetryPolicy {
    /// Create a new retry policy.
    pub fn new(max_retries: u32, backoff: BackoffStrategy) -> Self {
        Self {
            max_retries,
            backoff,
        }
    }

    /// No retries — execute once.
    pub fn no_retry() -> Self {
        Self {
            max_retries: 0,
            backoff: BackoffStrategy::Fixed { delay_ms: 0 },
        }
    }

    /// Compute the delay before the given attempt (0-indexed).
    pub fn delay(&self, attempt: u32) -> Duration {
        match &self.backoff {
            BackoffStrategy::Fixed { delay_ms } => Duration::from_millis(*delay_ms),
            BackoffStrategy::Exponential { base_ms, max_ms } => {
                let multiplier = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
                let delay = base_ms.saturating_mul(multiplier);
                Duration::from_millis(delay.min(*max_ms))
            }
            BackoffStrategy::Linear { base_ms, max_ms } => {
                let delay = base_ms.saturating_mul(u64::from(attempt) + 1);
                Duration::from_millis(delay.min(*max_ms))
            }
        }
    }

    /// Total maximum delay across all retries (for timeout budgeting).
    pub fn total_max_delay(&self) -> Duration {
        let retry_count = u128::from(self.max_retries);
        let max_duration_millis = Duration::MAX.as_millis();
        match &self.backoff {
            BackoffStrategy::Fixed { delay_ms } => {
                duration_from_millis_saturating(u128::from(*delay_ms).saturating_mul(retry_count))
            }
            BackoffStrategy::Linear { base_ms, max_ms } => {
                if self.max_retries == 0 || *base_ms == 0 || *max_ms == 0 {
                    return Duration::ZERO;
                }

                let uncapped_terms = retry_count.min(u128::from(*max_ms / *base_ms));
                let arithmetic_sum =
                    uncapped_terms.saturating_mul(uncapped_terms.saturating_add(1)) / 2;
                let mut total_millis = u128::from(*base_ms)
                    .saturating_mul(arithmetic_sum)
                    .min(max_duration_millis);

                let capped_terms = retry_count.saturating_sub(uncapped_terms);
                add_millis_saturating(
                    &mut total_millis,
                    u128::from(*max_ms).saturating_mul(capped_terms),
                );
                duration_from_millis_saturating(total_millis)
            }
            BackoffStrategy::Exponential { base_ms, max_ms } => {
                if self.max_retries == 0 || *base_ms == 0 || *max_ms == 0 {
                    return Duration::ZERO;
                }

                let mut total_millis = 0_u128;
                let mut attempt = 0_u32;
                while attempt < self.max_retries {
                    let delay_millis = match 1_u64.checked_shl(attempt) {
                        Some(multiplier) => base_ms.saturating_mul(multiplier).min(*max_ms),
                        None => *max_ms,
                    };
                    add_millis_saturating(&mut total_millis, u128::from(delay_millis));
                    attempt = attempt.saturating_add(1);

                    if delay_millis == *max_ms {
                        let remaining = u128::from(self.max_retries.saturating_sub(attempt));
                        add_millis_saturating(
                            &mut total_millis,
                            u128::from(*max_ms).saturating_mul(remaining),
                        );
                        break;
                    }
                }
                duration_from_millis_saturating(total_millis)
            }
        }
    }
}

/// Create a [`Cmd::Task`] that enforces a cooperative timeout.
///
/// The worker closure receives a [`CancellationToken`] and must honor it for
/// timely timeout teardown. On timeout, the runtime requests cancellation and
/// returns `on_timeout`; any late worker result is discarded.
pub fn task_with_timeout<M, F>(timeout: Duration, f: F, on_timeout: M) -> Cmd<M>
where
    M: Send + 'static,
    F: FnOnce(CancellationToken) -> M + Send + 'static,
{
    Cmd::task(move || {
        let source = CancellationSource::new();
        let token = source.token();
        let (handle, rx) = spawn_worker(move || f(token));
        match rx.recv_timeout(timeout) {
            Ok(result) => {
                join_task_thread(handle);
                // A panic is the task's, as it would be run inline.
                result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            }
            Err(_) => {
                source.cancel();
                join_task_thread_bounded(handle, "task_with_timeout");
                on_timeout
            }
        }
    })
}

/// Create a [`Cmd::Task`] with a named spec and cooperative timeout.
pub fn task_with_timeout_named<M, F>(
    name: impl Into<String>,
    timeout: Duration,
    f: F,
    on_timeout: M,
) -> Cmd<M>
where
    M: Send + 'static,
    F: FnOnce(CancellationToken) -> M + Send + 'static,
{
    Cmd::task_with_spec(TaskSpec::default().with_name(name), move || {
        let source = CancellationSource::new();
        let token = source.token();
        let (handle, rx) = spawn_worker(move || f(token));
        match rx.recv_timeout(timeout) {
            Ok(result) => {
                join_task_thread(handle);
                result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            }
            Err(_) => {
                source.cancel();
                join_task_thread_bounded(handle, "task_with_timeout_named");
                on_timeout
            }
        }
    })
}

/// Create a [`Cmd::Task`] that retries on failure with the given policy.
///
/// The `f` closure returns `Result<M, String>`. On `Ok`, the message is
/// returned immediately. On `Err`, the task retries according to the policy,
/// sleeping between attempts. After all retries are exhausted, `on_exhaust`
/// is called with the last error to produce a fallback message.
pub fn task_with_retry<M, F>(policy: RetryPolicy, f: F, on_exhaust: fn(String) -> M) -> Cmd<M>
where
    M: Send + 'static,
    F: Fn() -> Result<M, String> + Send + 'static,
{
    Cmd::task(move || {
        let mut last_err = String::new();
        for attempt in 0..=policy.max_retries {
            match f() {
                Ok(msg) => return msg,
                Err(e) => {
                    last_err = e;
                    if attempt < policy.max_retries {
                        std::thread::sleep(policy.delay(attempt));
                    }
                }
            }
        }
        on_exhaust(last_err)
    })
}

/// Create a [`Cmd::Task`] with both retry and timeout.
///
/// Each individual attempt is bounded by `per_attempt_timeout`. The worker
/// receives a [`CancellationToken`] and must honor it for timely timeout
/// teardown. The total number of attempts is governed by the retry policy.
pub fn task_with_retry_and_timeout<M, F>(
    policy: RetryPolicy,
    per_attempt_timeout: Duration,
    f: F,
    on_exhaust: fn(String) -> M,
) -> Cmd<M>
where
    M: Send + 'static,
    F: Fn(CancellationToken) -> Result<M, String> + Send + Sync + 'static,
{
    Cmd::task(move || {
        let f = std::sync::Arc::new(f);
        let mut last_err = String::new();
        for attempt in 0..=policy.max_retries {
            let source = CancellationSource::new();
            let token = source.token();
            let f_clone = std::sync::Arc::clone(&f);
            let (handle, rx) = spawn_worker(move || f_clone(token));
            match rx.recv_timeout(per_attempt_timeout) {
                Ok(Ok(Ok(msg))) => {
                    join_task_thread(handle);
                    return msg;
                }
                Ok(Ok(Err(e))) => {
                    join_task_thread(handle);
                    last_err = e;
                }
                // A panic is not a failed attempt to retry: it ends the task,
                // as it does `task_with_retry`, which runs `f` inline.
                Ok(Err(payload)) => {
                    join_task_thread(handle);
                    std::panic::resume_unwind(payload);
                }
                Err(_) => {
                    source.cancel();
                    join_task_thread_bounded(handle, "task_with_retry_and_timeout");
                    last_err = "timeout".into();
                }
            }
            if attempt < policy.max_retries {
                std::thread::sleep(policy.delay(attempt));
            }
        }
        on_exhaust(last_err)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_backoff_constant_delay() {
        let policy = RetryPolicy::new(3, BackoffStrategy::Fixed { delay_ms: 100 });
        assert_eq!(policy.delay(0), Duration::from_millis(100));
        assert_eq!(policy.delay(1), Duration::from_millis(100));
        assert_eq!(policy.delay(2), Duration::from_millis(100));
    }

    #[test]
    fn exponential_backoff_doubles() {
        let policy = RetryPolicy::new(
            5,
            BackoffStrategy::Exponential {
                base_ms: 100,
                max_ms: 5000,
            },
        );
        assert_eq!(policy.delay(0), Duration::from_millis(100));
        assert_eq!(policy.delay(1), Duration::from_millis(200));
        assert_eq!(policy.delay(2), Duration::from_millis(400));
        assert_eq!(policy.delay(3), Duration::from_millis(800));
    }

    #[test]
    fn exponential_backoff_caps_at_max() {
        let policy = RetryPolicy::new(
            5,
            BackoffStrategy::Exponential {
                base_ms: 1000,
                max_ms: 3000,
            },
        );
        assert_eq!(policy.delay(0), Duration::from_millis(1000));
        assert_eq!(policy.delay(1), Duration::from_millis(2000));
        assert_eq!(policy.delay(2), Duration::from_millis(3000)); // capped
        assert_eq!(policy.delay(3), Duration::from_millis(3000)); // capped
    }

    #[test]
    fn linear_backoff_increments() {
        let policy = RetryPolicy::new(
            4,
            BackoffStrategy::Linear {
                base_ms: 100,
                max_ms: 500,
            },
        );
        assert_eq!(policy.delay(0), Duration::from_millis(100));
        assert_eq!(policy.delay(1), Duration::from_millis(200));
        assert_eq!(policy.delay(2), Duration::from_millis(300));
        assert_eq!(policy.delay(3), Duration::from_millis(400));
        assert_eq!(policy.delay(4), Duration::from_millis(500)); // capped
    }

    #[test]
    fn linear_backoff_caps_at_max() {
        let policy = RetryPolicy::new(
            4,
            BackoffStrategy::Linear {
                base_ms: 200,
                max_ms: 500,
            },
        );
        assert_eq!(policy.delay(2), Duration::from_millis(500)); // 200*3 = 600, capped at 500
    }

    #[test]
    fn no_retry_policy() {
        let policy = RetryPolicy::no_retry();
        assert_eq!(policy.max_retries, 0);
    }

    #[test]
    fn total_max_delay_fixed() {
        let policy = RetryPolicy::new(3, BackoffStrategy::Fixed { delay_ms: 100 });
        assert_eq!(policy.total_max_delay(), Duration::from_millis(300));
    }

    #[test]
    fn total_max_delay_exponential() {
        let policy = RetryPolicy::new(
            3,
            BackoffStrategy::Exponential {
                base_ms: 100,
                max_ms: 10000,
            },
        );
        // Delays: 100 + 200 + 400 = 700
        assert_eq!(policy.total_max_delay(), Duration::from_millis(700));
    }

    #[test]
    fn total_max_delay_zero_retries() {
        let policy = RetryPolicy::no_retry();
        assert_eq!(policy.total_max_delay(), Duration::ZERO);
    }

    #[test]
    fn total_max_delay_fixed_saturates_without_iterating() {
        let policy = RetryPolicy::new(u32::MAX, BackoffStrategy::Fixed { delay_ms: u64::MAX });
        assert_eq!(policy.total_max_delay(), Duration::MAX);
    }

    #[test]
    fn total_max_delay_linear_handles_large_retry_counts() {
        let policy = RetryPolicy::new(
            u32::MAX,
            BackoffStrategy::Linear {
                base_ms: 1,
                max_ms: 10,
            },
        );
        assert_eq!(
            policy.total_max_delay(),
            Duration::from_millis(10_u64.saturating_mul(u64::from(u32::MAX)) - 45)
        );
    }

    #[test]
    fn total_max_delay_exponential_saturates_after_cap() {
        let policy = RetryPolicy::new(
            6,
            BackoffStrategy::Exponential {
                base_ms: 10,
                max_ms: 35,
            },
        );
        assert_eq!(
            policy.total_max_delay(),
            Duration::from_millis(10 + 20 + 35 * 4)
        );
    }

    #[test]
    fn total_max_delay_matches_delay_sequence_for_representative_policies() {
        let policies = [
            RetryPolicy::new(5, BackoffStrategy::Fixed { delay_ms: 7 }),
            RetryPolicy::new(
                6,
                BackoffStrategy::Linear {
                    base_ms: 3,
                    max_ms: 10,
                },
            ),
            RetryPolicy::new(
                4,
                BackoffStrategy::Linear {
                    base_ms: 10,
                    max_ms: 3,
                },
            ),
            RetryPolicy::new(
                6,
                BackoffStrategy::Exponential {
                    base_ms: 2,
                    max_ms: 9,
                },
            ),
        ];

        for policy in policies {
            let expected_millis = (0..policy.max_retries)
                .map(|attempt| policy.delay(attempt).as_millis())
                .sum::<u128>();
            assert_eq!(policy.total_max_delay().as_millis(), expected_millis);
        }
    }

    #[test]
    fn exponential_backoff_overflow_saturates() {
        let policy = RetryPolicy::new(
            1,
            BackoffStrategy::Exponential {
                base_ms: u64::MAX / 2,
                max_ms: u64::MAX,
            },
        );
        // Should not panic on overflow
        let _ = policy.delay(30);
    }

    #[test]
    fn linear_backoff_overflow_saturates() {
        let policy = RetryPolicy::new(
            1,
            BackoffStrategy::Linear {
                base_ms: u64::MAX / 2,
                max_ms: u64::MAX,
            },
        );
        let _ = policy.delay(30);
    }

    #[test]
    fn retry_policy_clone_eq() {
        let policy = RetryPolicy::new(
            3,
            BackoffStrategy::Exponential {
                base_ms: 100,
                max_ms: 5000,
            },
        );
        let cloned = policy.clone();
        assert_eq!(policy, cloned);
    }

    #[test]
    fn task_with_retry_succeeds_first_try() {
        #[derive(Debug, PartialEq)]
        enum Msg {
            Ok(i32),
            Err(String),
        }

        let policy = RetryPolicy::new(3, BackoffStrategy::Fixed { delay_ms: 1 });
        let cmd = task_with_retry(policy, || Ok(Msg::Ok(42)), Msg::Err);

        // Verify it produces a Task variant
        assert_eq!(cmd.type_name(), "Task");
    }

    #[test]
    fn task_with_timeout_produces_task() {
        #[derive(Debug)]
        #[allow(dead_code)]
        enum Msg {
            Result(i32),
            Timeout,
        }

        let cmd = task_with_timeout(
            Duration::from_secs(1),
            |_token| Msg::Result(42),
            Msg::Timeout,
        );
        assert_eq!(cmd.type_name(), "Task");
    }

    #[test]
    fn task_with_timeout_requests_cancellation_on_timeout() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Debug, PartialEq)]
        enum Msg {
            Finished,
            Timeout,
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::new(AtomicBool::new(false));
        let cancelled_flag = Arc::clone(&cancelled);
        let exited_flag = Arc::clone(&worker_exited);

        let cmd = task_with_timeout(
            Duration::from_millis(10),
            move |token| {
                cancelled_flag.store(token.wait_timeout(Duration::from_secs(1)), Ordering::SeqCst);
                exited_flag.store(true, Ordering::SeqCst);
                Msg::Finished
            },
            Msg::Timeout,
        );

        let result = match cmd {
            Cmd::Task(_, task) => task(),
            other => panic!("expected Task, got {other:?}"),
        };

        assert_eq!(result, Msg::Timeout);
        std::thread::sleep(Duration::from_millis(50));
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(worker_exited.load(Ordering::SeqCst));
    }

    #[cfg(not(panic = "abort"))]
    #[test]
    fn a_panicking_worker_is_the_tasks_panic_and_spares_the_terminal() {
        use std::cell::Cell;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Only panics on a worker this test marked count, so panics in
        // parallel tests cannot interfere.
        thread_local! {
            static MARKED_WORKER: Cell<bool> = const { Cell::new(false) };
        }
        static CLEANUPS: AtomicUsize = AtomicUsize::new(0);
        fn count_cleanup() {
            if MARKED_WORKER.with(Cell::get) {
                CLEANUPS.fetch_add(1, Ordering::SeqCst);
            }
        }
        ftui_core::session_teardown::install_chained_panic_hook(
            "retry-worker-panic-test",
            count_cleanup,
        );

        let cmd = task_with_timeout(
            Duration::from_secs(5),
            |_token| -> u8 {
                MARKED_WORKER.with(|marked| marked.set(true));
                panic!("worker bug")
            },
            0,
        );
        let Cmd::Task(_, task) = cmd else {
            panic!("expected a Task");
        };
        let start = std::time::Instant::now();
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task))
            .expect_err("the worker's panic should be the task's, not a timeout message");

        assert_eq!(payload.downcast_ref::<&str>(), Some(&"worker bug"));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(
            CLEANUPS.load(Ordering::SeqCst),
            0,
            "the worker's panic ran terminal cleanup"
        );
    }

    #[cfg(not(panic = "abort"))]
    #[test]
    fn a_panic_is_not_a_failed_attempt_to_retry() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_seen = Arc::clone(&attempts);
        let cmd = task_with_retry_and_timeout(
            RetryPolicy::new(3, BackoffStrategy::Fixed { delay_ms: 0 }),
            Duration::from_secs(5),
            move |_token| -> Result<u8, String> {
                attempts_seen.fetch_add(1, Ordering::SeqCst);
                panic!("worker bug")
            },
            |_err| 0,
        );
        let Cmd::Task(_, task) = cmd else {
            panic!("expected a Task");
        };

        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "a panic was retried");
    }

    #[test]
    fn task_with_retry_and_timeout_cancels_each_timed_out_attempt() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug, PartialEq)]
        enum Msg {
            Exhausted(String),
        }

        fn on_exhaust(err: String) -> Msg {
            Msg::Exhausted(err)
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        let attempts_flag = Arc::clone(&attempts);
        let cancelled_flag = Arc::clone(&cancelled);
        let policy = RetryPolicy::new(1, BackoffStrategy::Fixed { delay_ms: 0 });

        let cmd = task_with_retry_and_timeout(
            policy,
            Duration::from_millis(10),
            move |token| {
                attempts_flag.fetch_add(1, Ordering::SeqCst);
                if token.wait_timeout(Duration::from_secs(1)) {
                    cancelled_flag.fetch_add(1, Ordering::SeqCst);
                }
                Err("cancelled".to_owned())
            },
            on_exhaust,
        );

        let result = match cmd {
            Cmd::Task(_, task) => task(),
            other => panic!("expected Task, got {other:?}"),
        };

        assert_eq!(result, Msg::Exhausted("timeout".to_owned()));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(cancelled.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn backoff_strategy_variants_debug() {
        let fixed = BackoffStrategy::Fixed { delay_ms: 100 };
        let exp = BackoffStrategy::Exponential {
            base_ms: 100,
            max_ms: 5000,
        };
        let linear = BackoffStrategy::Linear {
            base_ms: 100,
            max_ms: 500,
        };
        // Just verify Debug doesn't panic
        let _ = format!("{fixed:?}");
        let _ = format!("{exp:?}");
        let _ = format!("{linear:?}");
    }
}
