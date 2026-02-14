//! Capability context (`Cx`) for cooperative cancellation and deadline propagation.
//!
//! `Cx` is a lightweight handle threaded through every async operation, I/O call,
//! and timer in the FrankenTUI stack. It enables:
//!
//! - **Cooperative cancellation**: Any holder can check `cx.is_cancelled()` and
//!   bail out early.
//! - **Deadline propagation**: A parent context's deadline flows to children.
//!   `cx.deadline()` returns the tightest deadline in the chain.
//! - **Deterministic testing via Lab**: In `Lab` mode, time is controlled
//!   externally, enabling fully reproducible test runs.
//!
//! # Design
//!
//! `Cx` is cheaply cloneable (`Arc` inside) and immutable from the outside.
//! To cancel or advance Lab time, hold the companion [`CxController`].
//!
//! # Tracing
//!
//! When the `tracing` feature is active, cancellation emits a `WARN`-level event
//! and deadline checks emit `TRACE`-level spans with `cx_id` and
//! `deadline_remaining_us` fields.
//!
//! # Example
//!
//! ```
//! use ftui_core::cx::{Cx, CxController};
//! use web_time::Duration;
//!
//! // Create a root context with a 500ms deadline.
//! let (cx, ctrl) = Cx::with_deadline(Duration::from_millis(500));
//! assert!(!cx.is_cancelled());
//! assert!(cx.deadline().is_some());
//!
//! // Cancel it.
//! ctrl.cancel();
//! assert!(cx.is_cancelled());
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use web_time::{Duration, Instant};

// Import tracing macros (no-op when tracing feature is disabled).
#[cfg(feature = "tracing")]
use crate::logging::warn;
#[cfg(not(feature = "tracing"))]
use crate::warn;

// ─── Cx ID generation ────────────────────────────────────────────────────────

static NEXT_CX_ID: AtomicU64 = AtomicU64::new(1);

fn next_cx_id() -> u64 {
    NEXT_CX_ID.fetch_add(1, Ordering::Relaxed)
}

// ─── Metrics counters ────────────────────────────────────────────────────────

/// Total number of Cx cancellations observed.
static CX_CANCELLATIONS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Read the total cancellation count (for diagnostics/telemetry).
#[must_use]
pub fn cx_cancellations_total() -> u64 {
    CX_CANCELLATIONS_TOTAL.load(Ordering::Relaxed)
}

// ─── Time source ─────────────────────────────────────────────────────────────

/// Time source abstraction for deterministic testing.
///
/// In production, `Cx` uses `web_time::Instant::now()`.
/// In Lab mode, time is controlled via [`LabClock`].
#[derive(Debug, Clone)]
enum TimeSource {
    /// Real wall-clock time.
    Real,
    /// Deterministic lab clock for testing.
    Lab(LabClock),
}

/// A manually-advanceable clock for deterministic tests.
///
/// All `Cx` instances sharing the same `LabClock` see the same time.
#[derive(Debug, Clone)]
pub struct LabClock {
    epoch: Instant,
    offset_us: Arc<AtomicU64>,
}

impl LabClock {
    /// Create a new lab clock starting at `Instant::now()`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            offset_us: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Advance the lab clock by `delta`.
    pub fn advance(&self, delta: Duration) {
        let us = delta.as_micros().min(u64::MAX as u128) as u64;
        self.offset_us.fetch_add(us, Ordering::Release);
    }

    /// Current lab time.
    #[must_use]
    pub fn now(&self) -> Instant {
        let offset = Duration::from_micros(self.offset_us.load(Ordering::Acquire));
        self.epoch + offset
    }
}

impl Default for LabClock {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Inner shared state ──────────────────────────────────────────────────────

#[derive(Debug)]
struct CxInner {
    id: u64,
    cancelled: AtomicBool,
    /// Deadline as microseconds since `created_at`. `u64::MAX` means no deadline.
    deadline_us: u64,
    created_at: Instant,
    time_source: TimeSource,
    /// Optional parent for deadline chain resolution.
    parent: Option<Arc<CxInner>>,
}

// ─── Cx ──────────────────────────────────────────────────────────────────────

/// Capability context handle.
///
/// Cheaply cloneable. Check `is_cancelled()` at natural yield points
/// (loop iterations, before I/O, before expensive computation).
#[derive(Clone, Debug)]
pub struct Cx {
    inner: Arc<CxInner>,
}

impl Cx {
    // ── Constructors ─────────────────────────────────────────────────

    /// Create a root context with no deadline.
    #[must_use]
    pub fn background() -> (Self, CxController) {
        Self::new_inner(u64::MAX, TimeSource::Real, None)
    }

    /// Create a root context with a deadline.
    #[must_use]
    pub fn with_deadline(deadline: Duration) -> (Self, CxController) {
        let us = deadline.as_micros().min(u64::MAX as u128) as u64;
        Self::new_inner(us, TimeSource::Real, None)
    }

    /// Create a root context using a [`LabClock`] for deterministic testing.
    #[must_use]
    pub fn lab(clock: &LabClock) -> (Self, CxController) {
        Self::new_inner(u64::MAX, TimeSource::Lab(clock.clone()), None)
    }

    /// Create a lab context with a deadline.
    #[must_use]
    pub fn lab_with_deadline(clock: &LabClock, deadline: Duration) -> (Self, CxController) {
        let us = deadline.as_micros().min(u64::MAX as u128) as u64;
        Self::new_inner(us, TimeSource::Lab(clock.clone()), None)
    }

    /// Derive a child context with a tighter deadline.
    ///
    /// The effective deadline is `min(parent.deadline(), child_deadline)`.
    /// Cancelling the parent also cancels the child (checked via chain walk).
    #[must_use]
    pub fn child(&self, deadline: Duration) -> (Self, CxController) {
        let us = deadline.as_micros().min(u64::MAX as u128) as u64;
        let time_source = match &self.inner.time_source {
            TimeSource::Real => TimeSource::Real,
            TimeSource::Lab(c) => TimeSource::Lab(c.clone()),
        };
        Self::new_inner(us, time_source, Some(self.inner.clone()))
    }

    /// Derive a child context that inherits the parent deadline.
    #[must_use]
    pub fn child_inherit(&self) -> (Self, CxController) {
        let time_source = match &self.inner.time_source {
            TimeSource::Real => TimeSource::Real,
            TimeSource::Lab(c) => TimeSource::Lab(c.clone()),
        };
        Self::new_inner(u64::MAX, time_source, Some(self.inner.clone()))
    }

    fn new_inner(
        deadline_us: u64,
        time_source: TimeSource,
        parent: Option<Arc<CxInner>>,
    ) -> (Self, CxController) {
        let now = match &time_source {
            TimeSource::Real => Instant::now(),
            TimeSource::Lab(c) => c.now(),
        };
        let inner = Arc::new(CxInner {
            id: next_cx_id(),
            cancelled: AtomicBool::new(false),
            deadline_us,
            created_at: now,
            time_source,
            parent,
        });
        let cx = Self {
            inner: inner.clone(),
        };
        let ctrl = CxController { inner };
        (cx, ctrl)
    }

    // ── Queries ──────────────────────────────────────────────────────

    /// Unique identifier for this context (for tracing/logging).
    #[inline]
    #[must_use]
    pub fn id(&self) -> u64 {
        self.inner.id
    }

    /// Check if this context (or any ancestor) has been cancelled.
    #[inline]
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.is_cancelled_inner(&self.inner)
    }

    fn is_cancelled_inner(&self, inner: &CxInner) -> bool {
        if inner.cancelled.load(Ordering::Acquire) {
            return true;
        }
        if let Some(ref parent) = inner.parent {
            return self.is_cancelled_inner(parent);
        }
        false
    }

    /// Check if the deadline has passed.
    #[inline]
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.remaining().is_some_and(|d| d.is_zero())
    }

    /// Check if the context is done (cancelled or expired).
    #[inline]
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.is_cancelled() || self.is_expired()
    }

    /// Return the effective deadline as a `Duration` from context creation,
    /// considering the full parent chain. Returns `None` if no deadline is set.
    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        let own = self.inner.deadline_us;
        let parent_remaining = self.parent_remaining_us();

        let now = self.now();
        let elapsed = now
            .checked_duration_since(self.inner.created_at)
            .unwrap_or(Duration::ZERO);
        let elapsed_us = elapsed.as_micros().min(u64::MAX as u128) as u64;

        // Own remaining
        let own_remaining = if own == u64::MAX {
            u64::MAX
        } else {
            own.saturating_sub(elapsed_us)
        };

        let effective = own_remaining.min(parent_remaining);
        if effective == u64::MAX {
            None
        } else {
            Some(Duration::from_micros(effective))
        }
    }

    /// Remaining time until deadline (saturates to zero, never negative).
    /// Returns `None` if no deadline is set.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline()
    }

    /// Remaining time in microseconds, or `None` if no deadline.
    #[must_use]
    pub fn remaining_us(&self) -> Option<u64> {
        self.remaining()
            .map(|d| d.as_micros().min(u64::MAX as u128) as u64)
    }

    /// Current time according to this context's time source.
    #[must_use]
    pub fn now(&self) -> Instant {
        match &self.inner.time_source {
            TimeSource::Real => Instant::now(),
            TimeSource::Lab(c) => c.now(),
        }
    }

    /// Whether this context uses a lab clock.
    #[inline]
    #[must_use]
    pub fn is_lab(&self) -> bool {
        matches!(self.inner.time_source, TimeSource::Lab(_))
    }

    fn parent_remaining_us(&self) -> u64 {
        match &self.inner.parent {
            Some(parent) => {
                let parent_cx = Cx {
                    inner: parent.clone(),
                };
                parent_cx.remaining_us().unwrap_or(u64::MAX)
            }
            None => u64::MAX,
        }
    }

    // ── Convenience ──────────────────────────────────────────────────

    /// Sleep for the given duration, respecting cancellation and deadline.
    ///
    /// Returns `true` if the full duration elapsed, `false` if cancelled or
    /// deadline expired early.
    pub fn sleep(&self, duration: Duration) -> bool {
        let effective = match self.remaining() {
            Some(rem) => duration.min(rem),
            None => duration,
        };
        if effective.is_zero() || self.is_cancelled() {
            return false;
        }

        // Use small sleep chunks for responsive cancellation checking
        let chunk = Duration::from_millis(10);
        let mut remaining = effective;
        while remaining > Duration::ZERO && !self.is_cancelled() {
            let sleep_time = remaining.min(chunk);
            std::thread::sleep(sleep_time);
            remaining = remaining.saturating_sub(sleep_time);
        }
        !self.is_cancelled() && remaining.is_zero()
    }
}

// ─── CxController ────────────────────────────────────────────────────────────

/// Control handle for a [`Cx`].
///
/// Held by the owner of the context to trigger cancellation.
/// Dropping the controller does **not** cancel the context — cancellation
/// is always explicit.
#[derive(Debug)]
pub struct CxController {
    inner: Arc<CxInner>,
}

impl CxController {
    /// Cancel the associated context.
    ///
    /// All clones of the `Cx` (and children) will observe `is_cancelled() == true`.
    pub fn cancel(&self) {
        let was_cancelled = self.inner.cancelled.swap(true, Ordering::Release);
        if !was_cancelled {
            CX_CANCELLATIONS_TOTAL.fetch_add(1, Ordering::Relaxed);
            warn!(cx_id = self.inner.id, "cx cancelled");
        }
    }

    /// Whether this context has already been cancelled.
    #[inline]
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }
}

// ─── CxError ─────────────────────────────────────────────────────────────────

/// Error returned when an operation is cancelled or times out via `Cx`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CxError {
    /// The context was explicitly cancelled.
    Cancelled,
    /// The context deadline expired.
    DeadlineExceeded,
}

impl std::fmt::Display for CxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "context cancelled"),
            Self::DeadlineExceeded => write!(f, "deadline exceeded"),
        }
    }
}

impl std::error::Error for CxError {}

impl Cx {
    /// Check if the context is still live; return `Err` if cancelled or expired.
    ///
    /// Intended for use at yield points:
    /// ```ignore
    /// cx.check()?;
    /// // ... continue work ...
    /// ```
    pub fn check(&self) -> Result<(), CxError> {
        if self.is_cancelled() {
            return Err(CxError::Cancelled);
        }
        if self.is_expired() {
            return Err(CxError::DeadlineExceeded);
        }
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_cx_is_not_cancelled() {
        let (cx, _ctrl) = Cx::background();
        assert!(!cx.is_cancelled());
        assert!(!cx.is_expired());
        assert!(!cx.is_done());
        assert!(cx.deadline().is_none());
    }

    #[test]
    fn cancel_propagates() {
        let (cx, ctrl) = Cx::background();
        assert!(!cx.is_cancelled());
        ctrl.cancel();
        assert!(cx.is_cancelled());
        assert!(cx.is_done());
    }

    #[test]
    fn clone_shares_cancellation() {
        let (cx, ctrl) = Cx::background();
        let cx2 = cx.clone();
        ctrl.cancel();
        assert!(cx.is_cancelled());
        assert!(cx2.is_cancelled());
    }

    #[test]
    fn deadline_reports_remaining() {
        let (cx, _ctrl) = Cx::with_deadline(Duration::from_secs(10));
        let rem = cx.remaining().expect("should have deadline");
        // Should be close to 10 seconds (minus tiny elapsed)
        assert!(rem.as_secs() >= 9);
    }

    #[test]
    fn child_inherits_cancellation() {
        let (parent, parent_ctrl) = Cx::background();
        let (child, _child_ctrl) = parent.child(Duration::from_secs(60));
        assert!(!child.is_cancelled());
        parent_ctrl.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_has_tighter_deadline() {
        let (parent, _) = Cx::with_deadline(Duration::from_secs(60));
        let (child, _) = parent.child(Duration::from_millis(100));
        let child_rem = child.remaining().expect("child has deadline");
        // Child deadline should be ~100ms, much less than parent's 60s
        assert!(child_rem < Duration::from_secs(1));
    }

    #[test]
    fn child_respects_parent_tighter_deadline() {
        let (parent, _) = Cx::with_deadline(Duration::from_millis(50));
        let (child, _) = parent.child(Duration::from_secs(60));
        let child_rem = child.remaining().expect("child has deadline via parent");
        // Parent deadline is tighter, child should see ~50ms
        assert!(child_rem < Duration::from_secs(1));
    }

    #[test]
    fn lab_clock_deterministic() {
        let clock = LabClock::new();
        let (cx, _ctrl) = Cx::lab_with_deadline(&clock, Duration::from_millis(100));

        // At t=0, should have ~100ms remaining
        let r1 = cx.remaining().expect("has deadline");
        assert!(r1 >= Duration::from_millis(90));

        // Advance 80ms
        clock.advance(Duration::from_millis(80));
        let r2 = cx.remaining().expect("has deadline");
        assert!(r2 <= Duration::from_millis(25));
        assert!(!cx.is_expired());

        // Advance past deadline
        clock.advance(Duration::from_millis(30));
        assert!(cx.is_expired());
        assert!(cx.is_done());
    }

    #[test]
    fn check_returns_ok_when_live() {
        let (cx, _ctrl) = Cx::background();
        assert!(cx.check().is_ok());
    }

    #[test]
    fn check_returns_cancelled() {
        let (cx, ctrl) = Cx::background();
        ctrl.cancel();
        assert_eq!(cx.check(), Err(CxError::Cancelled));
    }

    #[test]
    fn check_returns_deadline_exceeded() {
        let clock = LabClock::new();
        let (cx, _ctrl) = Cx::lab_with_deadline(&clock, Duration::from_millis(10));
        clock.advance(Duration::from_millis(20));
        assert_eq!(cx.check(), Err(CxError::DeadlineExceeded));
    }

    #[test]
    fn cx_id_is_unique() {
        let (cx1, _) = Cx::background();
        let (cx2, _) = Cx::background();
        assert_ne!(cx1.id(), cx2.id());
    }

    #[test]
    fn cx_is_lab() {
        let clock = LabClock::new();
        let (cx_lab, _) = Cx::lab(&clock);
        let (cx_real, _) = Cx::background();
        assert!(cx_lab.is_lab());
        assert!(!cx_real.is_lab());
    }

    #[test]
    fn child_inherit_no_deadline() {
        let (parent, _) = Cx::background();
        let (child, _) = parent.child_inherit();
        assert!(child.deadline().is_none());
    }

    #[test]
    fn child_inherit_with_parent_deadline() {
        let (parent, _) = Cx::with_deadline(Duration::from_secs(30));
        let (child, _) = parent.child_inherit();
        // Child has no own deadline but inherits parent's
        let rem = child.remaining().expect("inherits parent deadline");
        assert!(rem > Duration::from_secs(28));
    }

    #[test]
    fn cx_error_display() {
        assert_eq!(CxError::Cancelled.to_string(), "context cancelled");
        assert_eq!(CxError::DeadlineExceeded.to_string(), "deadline exceeded");
    }

    #[test]
    fn controller_is_cancelled_matches_cx() {
        let (cx, ctrl) = Cx::background();
        assert!(!ctrl.is_cancelled());
        ctrl.cancel();
        assert!(ctrl.is_cancelled());
        assert!(cx.is_cancelled());
    }

    #[test]
    fn double_cancel_is_idempotent() {
        let (cx, ctrl) = Cx::background();
        ctrl.cancel();
        ctrl.cancel();
        assert!(cx.is_cancelled());
    }

    #[test]
    fn lab_clock_advance_accumulates() {
        let clock = LabClock::new();
        let t0 = clock.now();
        clock.advance(Duration::from_millis(100));
        clock.advance(Duration::from_millis(200));
        let elapsed = clock.now().duration_since(t0);
        // Should be ~300ms
        assert!(elapsed >= Duration::from_millis(290));
        assert!(elapsed <= Duration::from_millis(310));
    }

    #[test]
    fn cancellation_counter_increments() {
        let before = cx_cancellations_total();
        let (_cx, ctrl) = Cx::background();
        ctrl.cancel();
        assert!(cx_cancellations_total() > before);
        // Double cancel should not increment again
        let after_first = cx_cancellations_total();
        ctrl.cancel();
        assert_eq!(cx_cancellations_total(), after_first);
    }

    #[test]
    fn sleep_respects_cancellation() {
        let (cx, ctrl) = Cx::background();
        // Cancel immediately so sleep returns false
        ctrl.cancel();
        let completed = cx.sleep(Duration::from_secs(10));
        assert!(!completed);
    }

    #[test]
    fn sleep_respects_lab_deadline() {
        let clock = LabClock::new();
        let (cx, _ctrl) = Cx::lab_with_deadline(&clock, Duration::from_millis(5));
        // Advance past deadline
        clock.advance(Duration::from_millis(10));
        let completed = cx.sleep(Duration::from_secs(10));
        assert!(!completed);
    }
}
