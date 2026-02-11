#![forbid(unsafe_code)]

//! Async validation with deterministic concurrency and token-based staleness prevention.
//!
//! This module provides infrastructure for async validators that:
//! - Use monotonic tokens to track input versions
//! - Prevent stale results from overriding newer input
//! - Record all validation events in a traceable log
//! - Support deterministic replay under fixed seeds
//!
//! # Design Principles
//!
//! 1. **Monotonic Tokens**: Each input change increments a token. Results include
//!    the token they were computed for.
//! 2. **Staleness Prevention**: Results are only applied if their token matches
//!    the current input token.
//! 3. **Event Tracing**: All validation lifecycle events are recorded for debugging
//!    and determinism verification.
//! 4. **Golden Trace Support**: Traces can be checksummed for regression testing.
//!
//! # Example
//!
//! ```rust,ignore
//! use ftui_extras::validation::async_validation::{
//!     AsyncValidationCoordinator, ValidationEvent, ValidationTrace
//! };
//!
//! let mut coordinator = AsyncValidationCoordinator::new();
//!
//! // Start validation for input
//! let token = coordinator.start_validation("user@example.com");
//!
//! // Simulate async validation completing
//! let result = ValidationResult::Valid;
//! coordinator.complete_validation(token, result);
//!
//! // Check trace for determinism
//! let trace = coordinator.trace();
//! assert!(trace.contains_event(ValidationEvent::Started { token }));
//! ```

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use web_time::{Duration, Instant};

use super::ValidationResult;

// ---------------------------------------------------------------------------
// ValidationToken
// ---------------------------------------------------------------------------

/// A monotonically increasing token representing a validation request version.
///
/// Tokens are used to detect stale validation results. When input changes,
/// a new token is issued. Results computed for older tokens are discarded.
///
/// # Invariants
///
/// - Tokens are strictly monotonic: `token_n < token_{n+1}`
/// - Token 0 is reserved for "no validation"
/// - Tokens never wrap (u64 provides sufficient headroom)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValidationToken(u64);

impl ValidationToken {
    /// The null token representing no validation.
    pub const NONE: Self = Self(0);

    /// Create a token from a raw value (for testing/deserialization).
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    /// Get the raw token value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Check if this is the null token.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

impl Default for ValidationToken {
    fn default() -> Self {
        Self::NONE
    }
}

impl std::fmt::Display for ValidationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Token({})", self.0)
    }
}

// ---------------------------------------------------------------------------
// ValidationEvent
// ---------------------------------------------------------------------------

/// An event in the validation lifecycle, recorded for tracing and debugging.
///
/// Events form a complete audit trail of validation activity, enabling:
/// - Debugging async validation issues
/// - Verifying deterministic behavior under replay
/// - Golden trace comparison for regression testing
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationEvent {
    /// Validation started for a token.
    Started {
        token: ValidationToken,
        /// Timestamp relative to coordinator creation (for determinism).
        elapsed_ns: u64,
    },

    /// Validation was cancelled (superseded by newer input).
    Cancelled {
        token: ValidationToken,
        /// The newer token that superseded this one.
        superseded_by: ValidationToken,
        elapsed_ns: u64,
    },

    /// Validation completed (may or may not be applied).
    Completed {
        token: ValidationToken,
        /// Whether the result was valid or invalid.
        is_valid: bool,
        /// Duration of the validation computation.
        duration_ns: u64,
        elapsed_ns: u64,
    },

    /// Validation result was applied to the state.
    Applied {
        token: ValidationToken,
        is_valid: bool,
        elapsed_ns: u64,
    },

    /// Validation result was discarded as stale.
    StaleDiscarded {
        token: ValidationToken,
        /// The current token when the result arrived.
        current_token: ValidationToken,
        elapsed_ns: u64,
    },
}

impl ValidationEvent {
    /// Get the token associated with this event.
    #[must_use]
    pub fn token(&self) -> ValidationToken {
        match self {
            Self::Started { token, .. }
            | Self::Cancelled { token, .. }
            | Self::Completed { token, .. }
            | Self::Applied { token, .. }
            | Self::StaleDiscarded { token, .. } => *token,
        }
    }

    /// Get the event type name for logging.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Cancelled { .. } => "cancelled",
            Self::Completed { .. } => "completed",
            Self::Applied { .. } => "applied",
            Self::StaleDiscarded { .. } => "stale_discarded",
        }
    }
}

impl Hash for ValidationEvent {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash discriminant and key fields for trace checksumming
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Started { token, elapsed_ns } => {
                token.hash(state);
                elapsed_ns.hash(state);
            }
            Self::Cancelled {
                token,
                superseded_by,
                elapsed_ns,
            } => {
                token.hash(state);
                superseded_by.hash(state);
                elapsed_ns.hash(state);
            }
            Self::Completed {
                token,
                is_valid,
                duration_ns,
                elapsed_ns,
            } => {
                token.hash(state);
                is_valid.hash(state);
                duration_ns.hash(state);
                elapsed_ns.hash(state);
            }
            Self::Applied {
                token,
                is_valid,
                elapsed_ns,
            } => {
                token.hash(state);
                is_valid.hash(state);
                elapsed_ns.hash(state);
            }
            Self::StaleDiscarded {
                token,
                current_token,
                elapsed_ns,
            } => {
                token.hash(state);
                current_token.hash(state);
                elapsed_ns.hash(state);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ValidationTrace
// ---------------------------------------------------------------------------

/// A trace of validation events for debugging and determinism verification.
///
/// Traces can be checksummed to verify that validation behavior is deterministic
/// across runs. This is useful for regression testing async validation logic.
#[derive(Debug, Clone, Default)]
pub struct ValidationTrace {
    events: Vec<ValidationEvent>,
}

impl ValidationTrace {
    /// Create a new empty trace.
    #[must_use]
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Add an event to the trace.
    pub fn push(&mut self, event: ValidationEvent) {
        self.events.push(event);
    }

    /// Get all events in the trace.
    #[must_use]
    pub fn events(&self) -> &[ValidationEvent] {
        &self.events
    }

    /// Check if the trace contains a specific event type for a token.
    #[must_use]
    pub fn contains_event_type(&self, token: ValidationToken, event_type: &str) -> bool {
        self.events
            .iter()
            .any(|e| e.token() == token && e.event_type() == event_type)
    }

    /// Get all events for a specific token.
    #[must_use]
    pub fn events_for_token(&self, token: ValidationToken) -> Vec<&ValidationEvent> {
        self.events.iter().filter(|e| e.token() == token).collect()
    }

    /// Compute a checksum of the trace for golden comparison.
    ///
    /// The checksum includes all event data and ordering, making it suitable
    /// for detecting any changes in validation behavior.
    #[must_use]
    pub fn checksum(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for event in &self.events {
            event.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Get the number of events in the trace.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Check if the trace is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Clear all events from the trace.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Verify trace invariants.
    ///
    /// Returns a list of violations if any invariants are broken.
    #[must_use]
    pub fn verify_invariants(&self) -> Vec<String> {
        let mut violations = Vec::new();

        // Invariant 1: Started events should have tokens in monotonic order
        let mut last_started_token = ValidationToken::NONE;
        for event in &self.events {
            if let ValidationEvent::Started { token, .. } = event {
                if *token <= last_started_token {
                    violations.push(format!(
                        "Non-monotonic start token: {} after {}",
                        token, last_started_token
                    ));
                }
                last_started_token = *token;
            }
        }

        // Invariant 2: Applied events should only occur for current token
        // (This is enforced by the coordinator, but we can check post-hoc)

        // Invariant 3: StaleDiscarded should have token < current_token
        for event in &self.events {
            if let ValidationEvent::StaleDiscarded {
                token,
                current_token,
                ..
            } = event
                && token >= current_token
            {
                violations.push(format!(
                    "StaleDiscarded with non-stale token: {} >= {}",
                    token, current_token
                ));
            }
        }

        violations
    }
}

// ---------------------------------------------------------------------------
// ValidationState
// ---------------------------------------------------------------------------

/// The state of an in-flight validation.
#[derive(Debug, Clone)]
pub struct InFlightValidation {
    /// The token for this validation.
    pub token: ValidationToken,
    /// When validation started (for duration tracking).
    pub started_at: Instant,
}

// ---------------------------------------------------------------------------
// AsyncValidationCoordinator
// ---------------------------------------------------------------------------

/// Coordinates async validations with token-based staleness prevention.
///
/// # Thread Safety
///
/// The coordinator is designed for single-threaded use with async validation
/// happening on background threads. The main thread calls `start_validation`
/// and `try_apply_result`, while background threads compute results.
///
/// # Determinism
///
/// For deterministic tracing, use `with_clock` to provide a fixed time source.
pub struct AsyncValidationCoordinator {
    /// The next token to issue.
    next_token: AtomicU64,

    /// The current (most recent) token.
    current_token: ValidationToken,

    /// Currently in-flight validations.
    in_flight: VecDeque<InFlightValidation>,

    /// The event trace.
    trace: ValidationTrace,

    /// When the coordinator was created (for elapsed time calculation).
    created_at: Instant,

    /// Optional fixed clock for deterministic testing (nanoseconds since start).
    fixed_clock: Option<Arc<AtomicU64>>,

    /// The most recently applied result.
    current_result: Option<ValidationResult>,
}

impl std::fmt::Debug for AsyncValidationCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncValidationCoordinator")
            .field("current_token", &self.current_token)
            .field("in_flight_count", &self.in_flight.len())
            .field("trace_events", &self.trace.len())
            .finish()
    }
}

impl Default for AsyncValidationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncValidationCoordinator {
    /// Create a new coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_token: AtomicU64::new(1),
            current_token: ValidationToken::NONE,
            in_flight: VecDeque::new(),
            trace: ValidationTrace::new(),
            created_at: Instant::now(),
            fixed_clock: None,
            current_result: None,
        }
    }

    /// Create a coordinator with a fixed clock for deterministic testing.
    ///
    /// The clock value represents nanoseconds since coordinator creation.
    #[must_use]
    pub fn with_fixed_clock(clock: Arc<AtomicU64>) -> Self {
        Self {
            next_token: AtomicU64::new(1),
            current_token: ValidationToken::NONE,
            in_flight: VecDeque::new(),
            trace: ValidationTrace::new(),
            created_at: Instant::now(),
            fixed_clock: Some(clock),
            current_result: None,
        }
    }

    /// Get the current elapsed time in nanoseconds.
    fn elapsed_ns(&self) -> u64 {
        self.fixed_clock.as_ref().map_or_else(
            || self.created_at.elapsed().as_nanos() as u64,
            |clock| clock.load(Ordering::SeqCst),
        )
    }

    /// Start a new validation, returning the token for this request.
    ///
    /// This cancels any in-flight validations with older tokens.
    pub fn start_validation(&mut self) -> ValidationToken {
        let token = ValidationToken(self.next_token.fetch_add(1, Ordering::SeqCst));
        let elapsed = self.elapsed_ns();

        // Cancel all in-flight validations (they're now stale)
        for validation in self.in_flight.drain(..) {
            self.trace.push(ValidationEvent::Cancelled {
                token: validation.token,
                superseded_by: token,
                elapsed_ns: elapsed,
            });
        }

        // Record the new validation
        self.in_flight.push_back(InFlightValidation {
            token,
            started_at: self.fixed_clock.as_ref().map_or_else(Instant::now, |_| {
                // For fixed clock, we just record the current instant
                // Duration will be calculated from elapsed_ns differences
                Instant::now()
            }),
        });

        self.current_token = token;
        self.trace.push(ValidationEvent::Started {
            token,
            elapsed_ns: elapsed,
        });

        token
    }

    /// Get the current token.
    #[must_use]
    pub fn current_token(&self) -> ValidationToken {
        self.current_token
    }

    /// Try to apply a validation result.
    ///
    /// Returns `true` if the result was applied (token matches current).
    /// Returns `false` if the result was discarded as stale.
    pub fn try_apply_result(
        &mut self,
        token: ValidationToken,
        result: ValidationResult,
        duration: Duration,
    ) -> bool {
        let elapsed = self.elapsed_ns();
        let is_valid = result.is_valid();
        let duration_ns = duration.as_nanos() as u64;

        // Record completion
        self.trace.push(ValidationEvent::Completed {
            token,
            is_valid,
            duration_ns,
            elapsed_ns: elapsed,
        });

        // Remove from in-flight
        self.in_flight.retain(|v| v.token != token);

        // Check if stale
        if token < self.current_token {
            self.trace.push(ValidationEvent::StaleDiscarded {
                token,
                current_token: self.current_token,
                elapsed_ns: elapsed,
            });
            return false;
        }

        // Apply the result
        self.current_result = Some(result);
        self.trace.push(ValidationEvent::Applied {
            token,
            is_valid,
            elapsed_ns: elapsed,
        });

        true
    }

    /// Get the current validation result.
    #[must_use]
    pub fn current_result(&self) -> Option<&ValidationResult> {
        self.current_result.as_ref()
    }

    /// Get the event trace.
    #[must_use]
    pub fn trace(&self) -> &ValidationTrace {
        &self.trace
    }

    /// Get a mutable reference to the trace.
    pub fn trace_mut(&mut self) -> &mut ValidationTrace {
        &mut self.trace
    }

    /// Clear the trace (for reuse).
    pub fn clear_trace(&mut self) {
        self.trace.clear();
    }

    /// Get the number of in-flight validations.
    #[must_use]
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Check if there are any in-flight validations.
    #[must_use]
    pub fn has_in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Verify that the trace satisfies all invariants.
    ///
    /// Returns `Ok(())` if valid, or `Err` with violation descriptions.
    pub fn verify_trace(&self) -> Result<(), Vec<String>> {
        let violations = self.trace.verify_invariants();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
}

// ---------------------------------------------------------------------------
// AsyncValidator Trait
// ---------------------------------------------------------------------------

/// A validator that can perform async validation.
///
/// Unlike the synchronous `Validator` trait, this trait is designed for
/// validations that may take significant time (network calls, complex checks).
///
/// # Implementation Note
///
/// Implementations should be stateless and side-effect free. The coordinator
/// handles all state management and staleness prevention.
pub trait AsyncValidator<T: ?Sized>: Send + Sync {
    /// Perform validation asynchronously.
    ///
    /// This method will be called on a background thread. It should not
    /// access shared mutable state.
    fn validate(&self, value: &T) -> ValidationResult;

    /// Return the default error message for this validator.
    fn error_message(&self) -> &str;

    /// Return an estimated duration for this validation (for scheduling).
    ///
    /// This is a hint for the coordinator to prioritize fast validators.
    fn estimated_duration(&self) -> Duration {
        Duration::from_millis(100) // Default: 100ms
    }
}

// ---------------------------------------------------------------------------
// Thread-Safe Coordinator Wrapper
// ---------------------------------------------------------------------------

/// A thread-safe wrapper around `AsyncValidationCoordinator`.
///
/// This allows the coordinator to be shared between the main thread
/// (which starts validations) and background threads (which complete them).
pub struct SharedValidationCoordinator {
    inner: Arc<Mutex<AsyncValidationCoordinator>>,
}

impl Clone for SharedValidationCoordinator {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for SharedValidationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedValidationCoordinator {
    fn lock_inner(&self) -> MutexGuard<'_, AsyncValidationCoordinator> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // Best-effort recovery: keep coordinator usable after a panic.
                poisoned.into_inner()
            }
        }
    }

    /// Create a new shared coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AsyncValidationCoordinator::new())),
        }
    }

    /// Create a shared coordinator with a fixed clock.
    #[must_use]
    pub fn with_fixed_clock(clock: Arc<AtomicU64>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(AsyncValidationCoordinator::with_fixed_clock(
                clock,
            ))),
        }
    }

    /// Start a new validation.
    pub fn start_validation(&self) -> ValidationToken {
        self.lock_inner().start_validation()
    }

    /// Get the current token.
    #[must_use]
    pub fn current_token(&self) -> ValidationToken {
        self.lock_inner().current_token()
    }

    /// Try to apply a validation result.
    pub fn try_apply_result(
        &self,
        token: ValidationToken,
        result: ValidationResult,
        duration: Duration,
    ) -> bool {
        self.lock_inner().try_apply_result(token, result, duration)
    }

    /// Get the current result.
    #[must_use]
    pub fn current_result(&self) -> Option<ValidationResult> {
        self.lock_inner().current_result().cloned()
    }

    /// Get a copy of the trace.
    #[must_use]
    pub fn trace(&self) -> ValidationTrace {
        self.lock_inner().trace().clone()
    }

    /// Get the trace checksum.
    #[must_use]
    pub fn trace_checksum(&self) -> u64 {
        self.lock_inner().trace().checksum()
    }

    /// Verify trace invariants.
    pub fn verify_trace(&self) -> Result<(), Vec<String>> {
        self.lock_inner().verify_trace()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::ValidationError;
    use std::sync::atomic::AtomicU64;
    use std::thread;
    use std::time::Duration;

    // -- ValidationToken tests --

    #[test]
    fn token_none_is_zero() {
        assert_eq!(ValidationToken::NONE.raw(), 0);
        assert!(ValidationToken::NONE.is_none());
    }

    #[test]
    fn token_from_raw() {
        let token = ValidationToken::from_raw(42);
        assert_eq!(token.raw(), 42);
        assert!(!token.is_none());
    }

    #[test]
    fn token_ordering() {
        let t1 = ValidationToken::from_raw(1);
        let t2 = ValidationToken::from_raw(2);
        let t3 = ValidationToken::from_raw(3);

        assert!(t1 < t2);
        assert!(t2 < t3);
        assert!(t1 < t3);
    }

    #[test]
    fn token_display() {
        let token = ValidationToken::from_raw(123);
        assert_eq!(format!("{token}"), "Token(123)");
    }

    // -- unit_token_monotonic: tokens strictly increase per input --

    #[test]
    fn unit_token_monotonic() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        let mut tokens = Vec::new();
        for i in 0..10 {
            clock.store(i * 1000, Ordering::SeqCst);
            tokens.push(coordinator.start_validation());
        }

        // Verify strict monotonicity
        for i in 1..tokens.len() {
            assert!(
                tokens[i] > tokens[i - 1],
                "Token {} ({}) should be greater than token {} ({})",
                i,
                tokens[i],
                i - 1,
                tokens[i - 1]
            );
        }

        // Verify via trace
        let violations = coordinator.trace().verify_invariants();
        assert!(
            violations.is_empty(),
            "Invariant violations: {:?}",
            violations
        );
    }

    // -- unit_stale_result_ignored: older token never applies --

    #[test]
    fn unit_stale_result_ignored() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        // Start validation 1
        clock.store(1000, Ordering::SeqCst);
        let token1 = coordinator.start_validation();

        // Start validation 2 (supersedes 1)
        clock.store(2000, Ordering::SeqCst);
        let token2 = coordinator.start_validation();

        // Validation 1 completes (stale)
        clock.store(3000, Ordering::SeqCst);
        let applied1 = coordinator.try_apply_result(
            token1,
            ValidationResult::Invalid(ValidationError::new("test", "stale")),
            Duration::from_millis(100),
        );

        // Validation 2 completes (current)
        clock.store(4000, Ordering::SeqCst);
        let applied2 = coordinator.try_apply_result(
            token2,
            ValidationResult::Valid,
            Duration::from_millis(50),
        );

        // Verify stale was rejected, current was applied
        assert!(!applied1, "Stale result should not be applied");
        assert!(applied2, "Current result should be applied");

        // Verify current result is from token2
        assert!(coordinator.current_result().unwrap().is_valid());

        // Verify trace contains StaleDiscarded event
        assert!(
            coordinator
                .trace()
                .contains_event_type(token1, "stale_discarded"),
            "Trace should contain stale_discarded for token1"
        );

        // Verify trace invariants
        let violations = coordinator.trace().verify_invariants();
        assert!(
            violations.is_empty(),
            "Invariant violations: {:?}",
            violations
        );
    }

    // -- unit_trace_contains_all_events: no missing transitions --

    #[test]
    fn unit_trace_contains_all_events() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        // Start validation
        clock.store(1000, Ordering::SeqCst);
        let token = coordinator.start_validation();

        // Complete validation
        clock.store(2000, Ordering::SeqCst);
        coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(50));

        // Verify all events are present
        let trace = coordinator.trace();

        assert!(
            trace.contains_event_type(token, "started"),
            "Trace should contain started event"
        );
        assert!(
            trace.contains_event_type(token, "completed"),
            "Trace should contain completed event"
        );
        assert!(
            trace.contains_event_type(token, "applied"),
            "Trace should contain applied event"
        );

        // Verify event count
        let token_events = trace.events_for_token(token);
        assert_eq!(token_events.len(), 3, "Should have 3 events for token");
    }

    #[test]
    fn trace_contains_cancelled_events() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        // Start validation 1
        clock.store(1000, Ordering::SeqCst);
        let token1 = coordinator.start_validation();

        // Start validation 2 (cancels 1)
        clock.store(2000, Ordering::SeqCst);
        let _token2 = coordinator.start_validation();

        // Verify cancelled event
        assert!(
            coordinator.trace().contains_event_type(token1, "cancelled"),
            "Trace should contain cancelled event for token1"
        );
    }

    // -- Coordinator basic tests --

    #[test]
    fn coordinator_initial_state() {
        let coordinator = AsyncValidationCoordinator::new();
        assert_eq!(coordinator.current_token(), ValidationToken::NONE);
        assert!(coordinator.current_result().is_none());
        assert!(!coordinator.has_in_flight());
    }

    #[test]
    fn coordinator_start_updates_current() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let token = coordinator.start_validation();
        assert_eq!(coordinator.current_token(), token);
        assert!(coordinator.has_in_flight());
    }

    #[test]
    fn coordinator_apply_clears_in_flight() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let token = coordinator.start_validation();
        assert_eq!(coordinator.in_flight_count(), 1);

        coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(10));
        assert_eq!(coordinator.in_flight_count(), 0);
    }

    // -- Trace checksum tests --

    #[test]
    fn trace_checksum_deterministic() {
        let clock1 = Arc::new(AtomicU64::new(0));
        let clock2 = Arc::new(AtomicU64::new(0));

        let mut coord1 = AsyncValidationCoordinator::with_fixed_clock(clock1.clone());
        let mut coord2 = AsyncValidationCoordinator::with_fixed_clock(clock2.clone());

        // Perform identical operations
        for i in 0..5 {
            clock1.store(i * 1000, Ordering::SeqCst);
            clock2.store(i * 1000, Ordering::SeqCst);

            let t1 = coord1.start_validation();
            let t2 = coord2.start_validation();

            clock1.store((i * 1000) + 500, Ordering::SeqCst);
            clock2.store((i * 1000) + 500, Ordering::SeqCst);

            coord1.try_apply_result(t1, ValidationResult::Valid, Duration::from_millis(50));
            coord2.try_apply_result(t2, ValidationResult::Valid, Duration::from_millis(50));
        }

        // Checksums should match
        assert_eq!(
            coord1.trace().checksum(),
            coord2.trace().checksum(),
            "Identical operations should produce identical checksums"
        );
    }

    #[test]
    fn trace_checksum_differs_on_different_operations() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coord1 = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        clock.store(0, Ordering::SeqCst);
        let t1 = coord1.start_validation();
        clock.store(500, Ordering::SeqCst);
        coord1.try_apply_result(t1, ValidationResult::Valid, Duration::from_millis(50));

        let checksum1 = coord1.trace().checksum();

        // Different operations
        clock.store(1000, Ordering::SeqCst);
        let t2 = coord1.start_validation();
        clock.store(1500, Ordering::SeqCst);
        coord1.try_apply_result(
            t2,
            ValidationResult::Invalid(ValidationError::new("test", "error")),
            Duration::from_millis(50),
        );

        let checksum2 = coord1.trace().checksum();

        assert_ne!(
            checksum1, checksum2,
            "Different operations should produce different checksums"
        );
    }

    // -- SharedValidationCoordinator tests --

    #[test]
    fn shared_coordinator_thread_safe() {
        let coordinator = SharedValidationCoordinator::new();
        let coord_clone = coordinator.clone();

        // Start validation on main thread
        let token = coordinator.start_validation();

        // Complete on background thread
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(10))
        });

        let applied = handle.join().unwrap();
        assert!(applied, "Result should be applied from background thread");
        assert!(coordinator.current_result().unwrap().is_valid());
    }

    #[test]
    fn shared_coordinator_concurrent_starts() {
        let coordinator = SharedValidationCoordinator::new();

        // Rapid starts should all get unique tokens
        let tokens: Vec<_> = (0..100).map(|_| coordinator.start_validation()).collect();

        // All tokens should be unique
        let mut unique = tokens.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), tokens.len(), "All tokens should be unique");
    }

    // -- Invariant verification tests --

    #[test]
    fn verify_invariants_passes_for_valid_trace() {
        let mut coordinator = AsyncValidationCoordinator::new();

        for _ in 0..5 {
            let token = coordinator.start_validation();
            coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(10));
        }

        let result = coordinator.verify_trace();
        assert!(result.is_ok(), "Valid trace should pass verification");
    }

    // -- ValidationEvent tests --

    #[test]
    fn event_type_names() {
        let token = ValidationToken::from_raw(1);

        let started = ValidationEvent::Started {
            token,
            elapsed_ns: 0,
        };
        assert_eq!(started.event_type(), "started");

        let cancelled = ValidationEvent::Cancelled {
            token,
            superseded_by: ValidationToken::from_raw(2),
            elapsed_ns: 0,
        };
        assert_eq!(cancelled.event_type(), "cancelled");

        let completed = ValidationEvent::Completed {
            token,
            is_valid: true,
            duration_ns: 0,
            elapsed_ns: 0,
        };
        assert_eq!(completed.event_type(), "completed");

        let applied = ValidationEvent::Applied {
            token,
            is_valid: true,
            elapsed_ns: 0,
        };
        assert_eq!(applied.event_type(), "applied");

        let stale = ValidationEvent::StaleDiscarded {
            token,
            current_token: ValidationToken::from_raw(2),
            elapsed_ns: 0,
        };
        assert_eq!(stale.event_type(), "stale_discarded");
    }

    #[test]
    fn trace_new_is_empty() {
        let trace = ValidationTrace::new();
        assert!(trace.is_empty());
        assert_eq!(trace.len(), 0);
        assert!(trace.events().is_empty());
    }

    #[test]
    fn trace_clear_removes_events() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let token = coordinator.start_validation();
        coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(10));
        assert!(!coordinator.trace().is_empty());
        coordinator.clear_trace();
        assert!(coordinator.trace().is_empty());
    }

    #[test]
    fn trace_events_for_token_filters() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let t1 = coordinator.start_validation();
        coordinator.try_apply_result(t1, ValidationResult::Valid, Duration::from_millis(10));
        let t2 = coordinator.start_validation();
        coordinator.try_apply_result(t2, ValidationResult::Valid, Duration::from_millis(10));

        let events_t1 = coordinator.trace().events_for_token(t1);
        assert!(events_t1.iter().all(|e| e.token() == t1));
        let events_t2 = coordinator.trace().events_for_token(t2);
        assert!(events_t2.iter().all(|e| e.token() == t2));
    }

    #[test]
    fn trace_mut_allows_push() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let before = coordinator.trace().len();
        coordinator.trace_mut().push(ValidationEvent::Started {
            token: ValidationToken::from_raw(999),
            elapsed_ns: 0,
        });
        assert_eq!(coordinator.trace().len(), before + 1);
    }

    #[test]
    fn shared_coordinator_trace_checksum() {
        let coordinator = SharedValidationCoordinator::new();
        let t1 = coordinator.start_validation();
        coordinator.try_apply_result(t1, ValidationResult::Valid, Duration::from_millis(10));
        let checksum = coordinator.trace_checksum();
        // Calling again should be deterministic
        assert_eq!(checksum, coordinator.trace_checksum());
    }

    #[test]
    fn coordinator_current_result_after_invalid() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let token = coordinator.start_validation();
        coordinator.try_apply_result(
            token,
            ValidationResult::Invalid(ValidationError::new("field", "too short")),
            Duration::from_millis(10),
        );
        let result = coordinator.current_result().unwrap();
        assert!(!result.is_valid());
    }

    // =====================================================================
    // Edge-case tests added for bd-e95ux
    // =====================================================================

    // -- ValidationToken edge cases --

    #[test]
    fn token_default_is_none() {
        let token: ValidationToken = Default::default();
        assert!(token.is_none());
        assert_eq!(token, ValidationToken::NONE);
    }

    #[test]
    fn token_display_none() {
        assert_eq!(format!("{}", ValidationToken::NONE), "Token(0)");
    }

    #[test]
    fn token_hash_consistent() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ValidationToken::from_raw(1));
        set.insert(ValidationToken::from_raw(1));
        assert_eq!(set.len(), 1, "same token should hash to same bucket");
        set.insert(ValidationToken::from_raw(2));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn token_equality() {
        assert_eq!(ValidationToken::from_raw(42), ValidationToken::from_raw(42));
        assert_ne!(ValidationToken::from_raw(1), ValidationToken::from_raw(2));
    }

    #[test]
    fn token_none_is_less_than_any_real() {
        assert!(ValidationToken::NONE < ValidationToken::from_raw(1));
    }

    // -- ValidationEvent edge cases --

    #[test]
    fn event_clone() {
        let event = ValidationEvent::Started {
            token: ValidationToken::from_raw(1),
            elapsed_ns: 100,
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
    }

    #[test]
    fn event_token_extraction_all_variants() {
        let t = ValidationToken::from_raw(7);
        let events = [
            ValidationEvent::Started {
                token: t,
                elapsed_ns: 0,
            },
            ValidationEvent::Cancelled {
                token: t,
                superseded_by: ValidationToken::from_raw(8),
                elapsed_ns: 0,
            },
            ValidationEvent::Completed {
                token: t,
                is_valid: true,
                duration_ns: 0,
                elapsed_ns: 0,
            },
            ValidationEvent::Applied {
                token: t,
                is_valid: false,
                elapsed_ns: 0,
            },
            ValidationEvent::StaleDiscarded {
                token: t,
                current_token: ValidationToken::from_raw(10),
                elapsed_ns: 0,
            },
        ];
        for event in &events {
            assert_eq!(event.token(), t, "token extraction for {:?}", event);
        }
    }

    #[test]
    fn event_hash_differs_by_variant() {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let t = ValidationToken::from_raw(1);
        let started = ValidationEvent::Started {
            token: t,
            elapsed_ns: 0,
        };
        let applied = ValidationEvent::Applied {
            token: t,
            is_valid: true,
            elapsed_ns: 0,
        };
        let mut h1 = DefaultHasher::new();
        started.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        applied.hash(&mut h2);
        assert_ne!(
            h1.finish(),
            h2.finish(),
            "different variants should hash differently"
        );
    }

    // -- ValidationTrace edge cases --

    #[test]
    fn trace_verify_invariants_empty_is_clean() {
        let trace = ValidationTrace::new();
        assert!(trace.verify_invariants().is_empty());
    }

    #[test]
    fn trace_verify_invariants_non_monotonic_start() {
        let mut trace = ValidationTrace::new();
        trace.push(ValidationEvent::Started {
            token: ValidationToken::from_raw(5),
            elapsed_ns: 0,
        });
        trace.push(ValidationEvent::Started {
            token: ValidationToken::from_raw(3), // non-monotonic!
            elapsed_ns: 100,
        });
        let violations = trace.verify_invariants();
        assert!(!violations.is_empty(), "should detect non-monotonic tokens");
        assert!(
            violations[0].contains("Non-monotonic"),
            "violation: {}",
            violations[0]
        );
    }

    #[test]
    fn trace_verify_invariants_stale_discarded_non_stale() {
        let mut trace = ValidationTrace::new();
        trace.push(ValidationEvent::StaleDiscarded {
            token: ValidationToken::from_raw(5),
            current_token: ValidationToken::from_raw(3), // token >= current (bad!)
            elapsed_ns: 0,
        });
        let violations = trace.verify_invariants();
        assert!(
            !violations.is_empty(),
            "should detect non-stale StaleDiscarded"
        );
        assert!(violations[0].contains("non-stale"));
    }

    #[test]
    fn trace_checksum_empty_deterministic() {
        let t1 = ValidationTrace::new();
        let t2 = ValidationTrace::new();
        assert_eq!(t1.checksum(), t2.checksum());
    }

    #[test]
    fn trace_checksum_changes_after_push() {
        let mut trace = ValidationTrace::new();
        let empty_checksum = trace.checksum();
        trace.push(ValidationEvent::Started {
            token: ValidationToken::from_raw(1),
            elapsed_ns: 0,
        });
        assert_ne!(trace.checksum(), empty_checksum);
    }

    #[test]
    fn trace_clone_is_independent() {
        let mut trace = ValidationTrace::new();
        trace.push(ValidationEvent::Started {
            token: ValidationToken::from_raw(1),
            elapsed_ns: 0,
        });
        let cloned = trace.clone();
        trace.clear();
        assert!(trace.is_empty());
        assert_eq!(cloned.len(), 1, "clone should be independent");
    }

    #[test]
    fn trace_contains_event_type_returns_false_for_missing() {
        let trace = ValidationTrace::new();
        assert!(!trace.contains_event_type(ValidationToken::from_raw(1), "started"));
    }

    #[test]
    fn trace_events_for_nonexistent_token() {
        let trace = ValidationTrace::new();
        let events = trace.events_for_token(ValidationToken::from_raw(99));
        assert!(events.is_empty());
    }

    // -- AsyncValidationCoordinator edge cases --

    #[test]
    fn coordinator_debug_format() {
        let coordinator = AsyncValidationCoordinator::new();
        let debug = format!("{coordinator:?}");
        assert!(debug.contains("AsyncValidationCoordinator"));
        assert!(debug.contains("current_token"));
    }

    #[test]
    fn coordinator_default_trait() {
        let coordinator: AsyncValidationCoordinator = Default::default();
        assert_eq!(coordinator.current_token(), ValidationToken::NONE);
        assert!(!coordinator.has_in_flight());
    }

    #[test]
    fn coordinator_multiple_rapid_starts_cancel_all_previous() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        clock.store(1000, Ordering::SeqCst);
        let t1 = coordinator.start_validation();
        clock.store(2000, Ordering::SeqCst);
        let t2 = coordinator.start_validation();
        clock.store(3000, Ordering::SeqCst);
        let _t3 = coordinator.start_validation();

        // t1 should be cancelled when t2 started, t2 cancelled when t3 started
        let trace = coordinator.trace();
        assert!(trace.contains_event_type(t1, "cancelled"));
        assert!(trace.contains_event_type(t2, "cancelled"));
    }

    #[test]
    fn coordinator_in_flight_count_after_multiple_starts() {
        let mut coordinator = AsyncValidationCoordinator::new();

        // Each start cancels all previous, so in_flight should always be 1
        coordinator.start_validation();
        assert_eq!(coordinator.in_flight_count(), 1);
        coordinator.start_validation();
        assert_eq!(coordinator.in_flight_count(), 1);
        coordinator.start_validation();
        assert_eq!(coordinator.in_flight_count(), 1);
    }

    #[test]
    fn coordinator_apply_stale_then_current() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        clock.store(1000, Ordering::SeqCst);
        let t1 = coordinator.start_validation();
        clock.store(2000, Ordering::SeqCst);
        let t2 = coordinator.start_validation();

        // Apply stale t1 — should fail
        clock.store(3000, Ordering::SeqCst);
        assert!(!coordinator.try_apply_result(
            t1,
            ValidationResult::Valid,
            Duration::from_millis(100),
        ));

        // Apply current t2 — should succeed
        clock.store(4000, Ordering::SeqCst);
        assert!(coordinator.try_apply_result(
            t2,
            ValidationResult::Valid,
            Duration::from_millis(50),
        ));

        // In-flight should be empty
        assert_eq!(coordinator.in_flight_count(), 0);
    }

    #[test]
    fn coordinator_result_replaced_by_newer() {
        let mut coordinator = AsyncValidationCoordinator::new();

        let t1 = coordinator.start_validation();
        coordinator.try_apply_result(
            t1,
            ValidationResult::Invalid(ValidationError::new("code", "first")),
            Duration::from_millis(10),
        );
        assert!(!coordinator.current_result().unwrap().is_valid());

        let t2 = coordinator.start_validation();
        coordinator.try_apply_result(t2, ValidationResult::Valid, Duration::from_millis(10));
        assert!(coordinator.current_result().unwrap().is_valid());
    }

    #[test]
    fn coordinator_apply_same_token_twice() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        clock.store(1000, Ordering::SeqCst);
        let token = coordinator.start_validation();

        // First apply
        clock.store(2000, Ordering::SeqCst);
        let first =
            coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(50));
        assert!(first);

        // Second apply of same token — should still succeed since token == current_token
        clock.store(3000, Ordering::SeqCst);
        let second = coordinator.try_apply_result(
            token,
            ValidationResult::Invalid(ValidationError::new("code", "err")),
            Duration::from_millis(50),
        );
        assert!(second, "same token applied again should succeed");
    }

    #[test]
    fn coordinator_verify_trace_after_full_lifecycle() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        for i in 0..5 {
            clock.store(i * 2000, Ordering::SeqCst);
            let token = coordinator.start_validation();
            clock.store(i * 2000 + 1000, Ordering::SeqCst);
            coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(50));
        }

        assert!(coordinator.verify_trace().is_ok());
    }

    #[test]
    fn coordinator_elapsed_ns_uses_fixed_clock() {
        let clock = Arc::new(AtomicU64::new(42_000));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        let token = coordinator.start_validation();
        let trace = coordinator.trace();
        let first = &trace.events()[0];
        if let ValidationEvent::Started { elapsed_ns, .. } = first {
            assert_eq!(*elapsed_ns, 42_000);
        } else {
            assert!(
                matches!(first, ValidationEvent::Started { .. }),
                "expected Started event, got: {first:?}"
            );
        }

        // Advance clock
        clock.store(99_000, Ordering::SeqCst);
        coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(10));
        let events = coordinator.trace().events();
        let last = events.last().unwrap();
        if let ValidationEvent::Applied { elapsed_ns, .. } = last {
            assert_eq!(*elapsed_ns, 99_000);
        } else {
            assert!(
                matches!(last, ValidationEvent::Applied { .. }),
                "expected Applied event, got: {last:?}"
            );
        }
    }

    // -- SharedValidationCoordinator edge cases --

    #[test]
    fn shared_coordinator_default_trait() {
        let coord: SharedValidationCoordinator = Default::default();
        assert_eq!(coord.current_token(), ValidationToken::NONE);
    }

    #[test]
    fn shared_coordinator_with_fixed_clock() {
        let clock = Arc::new(AtomicU64::new(5000));
        let coordinator = SharedValidationCoordinator::with_fixed_clock(clock);
        let token = coordinator.start_validation();
        let trace = coordinator.trace();
        let first = &trace.events()[0];
        if let ValidationEvent::Started { elapsed_ns, .. } = first {
            assert_eq!(*elapsed_ns, 5000);
        } else {
            assert!(
                matches!(first, ValidationEvent::Started { .. }),
                "expected Started event, got: {first:?}"
            );
        }
        coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(10));
    }

    #[test]
    fn shared_coordinator_verify_trace() {
        let coordinator = SharedValidationCoordinator::new();
        let t = coordinator.start_validation();
        coordinator.try_apply_result(t, ValidationResult::Valid, Duration::from_millis(10));
        assert!(coordinator.verify_trace().is_ok());
    }

    #[test]
    fn shared_coordinator_multiple_threads_unique_tokens() {
        let coordinator = SharedValidationCoordinator::new();
        let mut handles = vec![];
        for _ in 0..10 {
            let c = coordinator.clone();
            handles.push(thread::spawn(move || c.start_validation()));
        }
        let tokens: Vec<ValidationToken> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let mut sorted = tokens.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            tokens.len(),
            "all tokens from concurrent threads should be unique"
        );
    }

    // -- InFlightValidation --

    #[test]
    fn in_flight_validation_debug() {
        let v = InFlightValidation {
            token: ValidationToken::from_raw(3),
            started_at: Instant::now(),
        };
        let debug = format!("{v:?}");
        assert!(debug.contains("InFlightValidation"));
        assert!(debug.contains("token"));
    }

    #[test]
    fn in_flight_validation_clone() {
        let v = InFlightValidation {
            token: ValidationToken::from_raw(5),
            started_at: Instant::now(),
        };
        let cloned = v.clone();
        assert_eq!(cloned.token, v.token);
    }

    // -- AsyncValidator trait --

    #[test]
    fn async_validator_estimated_duration_default() {
        struct TestValidator;
        impl AsyncValidator<str> for TestValidator {
            fn validate(&self, _value: &str) -> ValidationResult {
                ValidationResult::Valid
            }
            fn error_message(&self) -> &str {
                "test error"
            }
        }
        let v = TestValidator;
        assert_eq!(v.estimated_duration(), Duration::from_millis(100));
        assert_eq!(v.error_message(), "test error");
        assert!(v.validate("anything").is_valid());
    }

    #[test]
    fn async_validator_custom_estimated_duration() {
        struct SlowValidator;
        impl AsyncValidator<str> for SlowValidator {
            fn validate(&self, _value: &str) -> ValidationResult {
                ValidationResult::Valid
            }
            fn error_message(&self) -> &str {
                "slow"
            }
            fn estimated_duration(&self) -> Duration {
                Duration::from_secs(5)
            }
        }
        let v = SlowValidator;
        assert_eq!(v.estimated_duration(), Duration::from_secs(5));
    }

    // -- Coordinator with zero-duration results --

    #[test]
    fn coordinator_zero_duration_result() {
        let mut coordinator = AsyncValidationCoordinator::new();
        let token = coordinator.start_validation();
        let applied = coordinator.try_apply_result(token, ValidationResult::Valid, Duration::ZERO);
        assert!(applied);
    }

    // -- Trace with many events --

    #[test]
    fn trace_many_events() {
        let clock = Arc::new(AtomicU64::new(0));
        let mut coordinator = AsyncValidationCoordinator::with_fixed_clock(clock.clone());

        for i in 0..100 {
            clock.store(i * 100, Ordering::SeqCst);
            let token = coordinator.start_validation();
            clock.store(i * 100 + 50, Ordering::SeqCst);
            coordinator.try_apply_result(token, ValidationResult::Valid, Duration::from_millis(1));
        }

        let trace = coordinator.trace();
        // Each iteration: started + completed + applied = 3
        // (try_apply_result clears in-flight, so next start has nothing to cancel)
        assert_eq!(trace.len(), 100 * 3);
        assert!(coordinator.verify_trace().is_ok());
    }
}
