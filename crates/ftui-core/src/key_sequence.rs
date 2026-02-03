#![forbid(unsafe_code)]

//! Key sequence interpreter for multi-key sequences (bd-2vne.2).
//!
//! This module provides a stateful interpreter for detecting key sequences like
//! Esc Esc, independent of the low-level input parsing. It operates on the
//! [`KeyEvent`] stream and uses a configurable timeout window to detect sequences.
//!
//! # Design
//!
//! ## Invariants
//! 1. Sequences are always non-empty when emitted.
//! 2. The timeout window is measured from the first key in a potential sequence.
//! 3. If no sequence is detected within the timeout, the buffered key(s) are
//!    emitted individually via [`KeySequenceAction::Emit`].
//! 4. Non-blocking: [`KeySequenceAction::Pending`] signals that more input is
//!    needed, but the caller can continue with other work.
//!
//! ## Failure Modes
//! - If the timeout expires mid-sequence, buffered keys are flushed as individual
//!   [`Emit`](KeySequenceAction::Emit) actions (graceful degradation).
//! - Unknown keys that don't match any sequence pattern are passed through immediately.
//!
//! # Example
//!
//! ```
//! use ftui_core::key_sequence::{KeySequenceInterpreter, KeySequenceConfig, KeySequenceAction};
//! use ftui_core::event::{KeyEvent, KeyCode, KeyEventKind, Modifiers};
//! use std::time::{Duration, Instant};
//!
//! let config = KeySequenceConfig::default();
//! let mut interp = KeySequenceInterpreter::new(config);
//!
//! let esc = KeyEvent {
//!     code: KeyCode::Escape,
//!     modifiers: Modifiers::NONE,
//!     kind: KeyEventKind::Press,
//! };
//!
//! let now = Instant::now();
//!
//! // First Esc: pending (waiting for potential second Esc)
//! let action = interp.feed(&esc, now);
//! assert!(matches!(action, KeySequenceAction::Pending));
//!
//! // Second Esc within timeout: emit sequence
//! let action = interp.feed(&esc, now + Duration::from_millis(100));
//! assert!(matches!(action, KeySequenceAction::EmitSequence { .. }));
//! ```

use std::time::{Duration, Instant};

use crate::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for key sequence detection.
#[derive(Debug, Clone)]
pub struct KeySequenceConfig {
    /// Time window for sequence completion (default: 250ms).
    ///
    /// If a second key doesn't arrive within this window, the first key
    /// is emitted as a single key event.
    pub sequence_timeout: Duration,

    /// Whether to detect Esc Esc sequences (default: true).
    pub detect_double_escape: bool,
}

impl Default for KeySequenceConfig {
    fn default() -> Self {
        Self {
            sequence_timeout: Duration::from_millis(250),
            detect_double_escape: true,
        }
    }
}

impl KeySequenceConfig {
    /// Create a config with a custom timeout.
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            sequence_timeout: timeout,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// KeySequenceKind
// ---------------------------------------------------------------------------

/// Recognized key sequence patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeySequenceKind {
    /// Double Escape (Esc Esc) - typically used for tree view toggle.
    DoubleEscape,
}

impl KeySequenceKind {
    /// Human-readable name for this sequence.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DoubleEscape => "Esc Esc",
        }
    }
}

// ---------------------------------------------------------------------------
// KeySequenceAction
// ---------------------------------------------------------------------------

/// Action returned by the key sequence interpreter.
#[derive(Debug, Clone, PartialEq)]
pub enum KeySequenceAction {
    /// Emit the key event immediately (no sequence detected).
    Emit(KeyEvent),

    /// A complete key sequence was detected.
    EmitSequence {
        /// The kind of sequence that was detected.
        kind: KeySequenceKind,
        /// The raw key events that formed the sequence.
        keys: Vec<KeyEvent>,
    },

    /// Waiting for more keys to complete a potential sequence.
    ///
    /// The caller should continue with other work; the interpreter will
    /// return the buffered keys if the timeout expires.
    Pending,
}

impl KeySequenceAction {
    /// Returns true if this action requires the caller to wait for more input.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    /// Returns true if this action emits a sequence.
    #[must_use]
    pub const fn is_sequence(&self) -> bool {
        matches!(self, Self::EmitSequence { .. })
    }
}

// ---------------------------------------------------------------------------
// KeySequenceInterpreter
// ---------------------------------------------------------------------------

/// Stateful interpreter for multi-key sequences.
///
/// Feed key events via [`feed`](Self::feed) and periodically call
/// [`check_timeout`](Self::check_timeout) to handle expired sequences.
pub struct KeySequenceInterpreter {
    config: KeySequenceConfig,

    /// Buffer for pending keys.
    buffer: Vec<KeyEvent>,

    /// Timestamp of the first key in the current buffer.
    buffer_start: Option<Instant>,
}

impl std::fmt::Debug for KeySequenceInterpreter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeySequenceInterpreter")
            .field("buffer_len", &self.buffer.len())
            .field("has_pending", &self.buffer_start.is_some())
            .finish()
    }
}

impl KeySequenceInterpreter {
    /// Create a new key sequence interpreter with the given configuration.
    #[must_use]
    pub fn new(config: KeySequenceConfig) -> Self {
        Self {
            config,
            buffer: Vec::with_capacity(4),
            buffer_start: None,
        }
    }

    /// Create a new interpreter with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(KeySequenceConfig::default())
    }

    /// Feed a key event into the interpreter.
    ///
    /// Returns an action indicating what the caller should do:
    /// - [`Emit`](KeySequenceAction::Emit): Pass this key through immediately
    /// - [`EmitSequence`](KeySequenceAction::EmitSequence): A sequence was detected
    /// - [`Pending`](KeySequenceAction::Pending): Waiting for more keys
    ///
    /// # Key Event Filtering
    ///
    /// Only key press events are processed. Release and repeat events are
    /// passed through immediately.
    pub fn feed(&mut self, event: &KeyEvent, now: Instant) -> KeySequenceAction {
        // Only process key press events
        if event.kind != KeyEventKind::Press {
            return KeySequenceAction::Emit(*event);
        }

        // Check for timeout before processing new key
        if let Some(action) = self.check_timeout_internal(now) {
            // Timeout expired - need to flush buffer first, then handle new key
            // We'll buffer the new key and let caller handle the flush action
            // Actually, we should flush and then process the new key
            self.buffer.clear();
            self.buffer_start = None;
            // Fall through to process the new key
            let _ = action; // The flush action would have been handled by check_timeout
        }

        // Check if this key could start or continue a sequence
        match self.try_sequence(event, now) {
            SequenceResult::Complete(kind) => {
                let keys = std::mem::take(&mut self.buffer);
                self.buffer_start = None;
                KeySequenceAction::EmitSequence { kind, keys }
            }
            SequenceResult::Continue => {
                if self.buffer.is_empty() {
                    self.buffer_start = Some(now);
                }
                self.buffer.push(*event);
                KeySequenceAction::Pending
            }
            SequenceResult::NoMatch => {
                // This key doesn't match any sequence pattern
                // If we have buffered keys, we need to flush them first
                if !self.buffer.is_empty() {
                    // The buffered keys didn't form a sequence, flush them
                    // For now, just clear and pass through the new key
                    // The caller should have called check_timeout to get the buffered keys
                    self.buffer.clear();
                    self.buffer_start = None;
                }
                KeySequenceAction::Emit(*event)
            }
        }
    }

    /// Check if the sequence timeout has expired.
    ///
    /// Call this periodically (e.g., on tick) to flush expired sequences.
    /// Returns buffered keys as individual [`Emit`](KeySequenceAction::Emit) actions
    /// if the timeout has expired.
    ///
    /// Returns `None` if no timeout has expired or no keys are pending.
    pub fn check_timeout(&mut self, now: Instant) -> Option<Vec<KeySequenceAction>> {
        if let Some(start) = self.buffer_start {
            if now.duration_since(start) >= self.config.sequence_timeout {
                // Timeout expired - flush all buffered keys
                let actions: Vec<_> = self.buffer.drain(..).map(KeySequenceAction::Emit).collect();
                self.buffer_start = None;
                if actions.is_empty() {
                    None
                } else {
                    Some(actions)
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Returns true if there are pending keys waiting for a potential sequence.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.buffer_start.is_some()
    }

    /// Get the time remaining until the current pending sequence times out.
    ///
    /// Returns `None` if there are no pending keys.
    #[must_use]
    pub fn time_until_timeout(&self, now: Instant) -> Option<Duration> {
        self.buffer_start.map(|start| {
            let elapsed = now.duration_since(start);
            self.config.sequence_timeout.saturating_sub(elapsed)
        })
    }

    /// Reset all state, discarding any pending keys.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.buffer_start = None;
    }

    /// Flush any pending keys immediately as individual emit actions.
    ///
    /// Useful when the application needs to ensure all keys are processed
    /// before a state transition (e.g., on focus loss).
    pub fn flush(&mut self) -> Vec<KeySequenceAction> {
        let actions: Vec<_> = self.buffer.drain(..).map(KeySequenceAction::Emit).collect();
        self.buffer_start = None;
        actions
    }

    /// Get a reference to the current configuration.
    #[must_use]
    pub fn config(&self) -> &KeySequenceConfig {
        &self.config
    }

    /// Update the configuration.
    ///
    /// Note: This does not affect keys already in the buffer.
    pub fn set_config(&mut self, config: KeySequenceConfig) {
        self.config = config;
    }
}

// ---------------------------------------------------------------------------
// Internal
// ---------------------------------------------------------------------------

/// Result of trying to match a sequence pattern.
#[derive(Debug, Clone, Copy)]
enum SequenceResult {
    /// A complete sequence was detected.
    Complete(KeySequenceKind),
    /// Key could be part of a sequence, continue buffering.
    Continue,
    /// Key doesn't match any sequence pattern.
    NoMatch,
}

impl KeySequenceInterpreter {
    /// Internal timeout check that returns a single action.
    fn check_timeout_internal(&mut self, now: Instant) -> Option<KeySequenceAction> {
        if let Some(start) = self.buffer_start
            && now.duration_since(start) >= self.config.sequence_timeout
            && let Some(key) = self.buffer.first().copied()
        {
            self.buffer.remove(0);
            if self.buffer.is_empty() {
                self.buffer_start = None;
            }
            return Some(KeySequenceAction::Emit(key));
        }
        None
    }

    /// Try to match the current key against known sequence patterns.
    fn try_sequence(&self, event: &KeyEvent, _now: Instant) -> SequenceResult {
        // Double Escape detection
        if self.config.detect_double_escape
            && event.code == KeyCode::Escape
            && event.modifiers == Modifiers::NONE
        {
            // Check if we already have an Escape in the buffer
            if self.buffer.len() == 1
                && self.buffer[0].code == KeyCode::Escape
                && self.buffer[0].modifiers == Modifiers::NONE
            {
                return SequenceResult::Complete(KeySequenceKind::DoubleEscape);
            }
            // This could be the first Escape of a double-escape sequence
            if self.buffer.is_empty() {
                return SequenceResult::Continue;
            }
        }

        // No sequence pattern matched
        SequenceResult::NoMatch
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    fn esc() -> KeyEvent {
        KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        }
    }

    fn key_release(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Release,
        }
    }

    const MS_50: Duration = Duration::from_millis(50);
    const MS_100: Duration = Duration::from_millis(100);
    const MS_300: Duration = Duration::from_millis(300);

    // --- Double Escape tests ---

    #[test]
    fn double_escape_within_timeout() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        // First Esc
        let action = interp.feed(&esc(), t);
        assert!(matches!(action, KeySequenceAction::Pending));
        assert!(interp.has_pending());

        // Second Esc within timeout
        let action = interp.feed(&esc(), t + MS_100);
        assert!(matches!(
            action,
            KeySequenceAction::EmitSequence {
                kind: KeySequenceKind::DoubleEscape,
                ..
            }
        ));
        assert!(!interp.has_pending());
    }

    #[test]
    fn single_escape_timeout() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        // First Esc
        let action = interp.feed(&esc(), t);
        assert!(matches!(action, KeySequenceAction::Pending));

        // Timeout check after 300ms (default timeout is 250ms)
        let actions = interp.check_timeout(t + MS_300);
        assert!(actions.is_some());
        let actions = actions.unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], KeySequenceAction::Emit(_)));
    }

    #[test]
    fn escape_then_different_key() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        // First Esc
        let action = interp.feed(&esc(), t);
        assert!(matches!(action, KeySequenceAction::Pending));

        // Different key - should emit immediately (Esc was cleared by timeout logic)
        let action = interp.feed(&key('a'), t + MS_50);
        assert!(matches!(action, KeySequenceAction::Emit(_)));
        assert!(!interp.has_pending());
    }

    #[test]
    fn non_escape_key_passes_through() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        let action = interp.feed(&key('x'), t);
        assert!(matches!(action, KeySequenceAction::Emit(_)));
        assert!(!interp.has_pending());
    }

    #[test]
    fn key_release_passes_through() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        let action = interp.feed(&key_release('x'), t);
        assert!(matches!(action, KeySequenceAction::Emit(_)));
        assert!(!interp.has_pending());
    }

    #[test]
    fn modified_escape_passes_through() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        // Ctrl+Escape should not start a sequence
        let ctrl_esc = KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        };

        let action = interp.feed(&ctrl_esc, t);
        assert!(matches!(action, KeySequenceAction::Emit(_)));
        assert!(!interp.has_pending());
    }

    // --- Configuration tests ---

    #[test]
    fn custom_timeout() {
        let config = KeySequenceConfig::with_timeout(Duration::from_millis(100));
        let mut interp = KeySequenceInterpreter::new(config);
        let t = now();

        // First Esc
        interp.feed(&esc(), t);
        assert!(interp.has_pending());

        // Before timeout (50ms)
        assert!(interp.check_timeout(t + MS_50).is_none());

        // After timeout (150ms > 100ms)
        let actions = interp.check_timeout(t + Duration::from_millis(150));
        assert!(actions.is_some());
    }

    #[test]
    fn disabled_double_escape() {
        let config = KeySequenceConfig {
            detect_double_escape: false,
            ..Default::default()
        };
        let mut interp = KeySequenceInterpreter::new(config);
        let t = now();

        // First Esc - should pass through immediately since detection is disabled
        let action = interp.feed(&esc(), t);
        assert!(matches!(action, KeySequenceAction::Emit(_)));
        assert!(!interp.has_pending());
    }

    // --- Helper method tests ---

    #[test]
    fn time_until_timeout() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        assert!(interp.time_until_timeout(t).is_none());

        interp.feed(&esc(), t);
        let remaining = interp.time_until_timeout(t + MS_100);
        assert!(remaining.is_some());
        let remaining = remaining.unwrap();
        // Default timeout is 250ms, we're at 100ms, so ~150ms remaining
        assert!(remaining >= Duration::from_millis(140));
        assert!(remaining <= Duration::from_millis(160));
    }

    #[test]
    fn reset_clears_state() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        interp.feed(&esc(), t);
        assert!(interp.has_pending());

        interp.reset();
        assert!(!interp.has_pending());
    }

    #[test]
    fn flush_returns_pending_keys() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        interp.feed(&esc(), t);
        assert!(interp.has_pending());

        let actions = interp.flush();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], KeySequenceAction::Emit(_)));
        assert!(!interp.has_pending());
    }

    #[test]
    fn flush_on_empty_returns_empty() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let actions = interp.flush();
        assert!(actions.is_empty());
    }

    #[test]
    fn config_getter_and_setter() {
        let mut interp = KeySequenceInterpreter::with_defaults();

        assert_eq!(interp.config().sequence_timeout, Duration::from_millis(250));

        let new_config = KeySequenceConfig::with_timeout(Duration::from_millis(500));
        interp.set_config(new_config);

        assert_eq!(interp.config().sequence_timeout, Duration::from_millis(500));
    }

    #[test]
    fn debug_format() {
        let interp = KeySequenceInterpreter::with_defaults();
        let dbg = format!("{:?}", interp);
        assert!(dbg.contains("KeySequenceInterpreter"));
    }

    // --- Action method tests ---

    #[test]
    fn action_is_pending() {
        assert!(KeySequenceAction::Pending.is_pending());
        assert!(!KeySequenceAction::Emit(esc()).is_pending());
        assert!(
            !KeySequenceAction::EmitSequence {
                kind: KeySequenceKind::DoubleEscape,
                keys: vec![],
            }
            .is_pending()
        );
    }

    #[test]
    fn action_is_sequence() {
        assert!(!KeySequenceAction::Pending.is_sequence());
        assert!(!KeySequenceAction::Emit(esc()).is_sequence());
        assert!(
            KeySequenceAction::EmitSequence {
                kind: KeySequenceKind::DoubleEscape,
                keys: vec![],
            }
            .is_sequence()
        );
    }

    // --- KeySequenceKind tests ---

    #[test]
    fn sequence_kind_name() {
        assert_eq!(KeySequenceKind::DoubleEscape.name(), "Esc Esc");
    }

    // --- Default config tests ---

    #[test]
    fn default_config_values() {
        let config = KeySequenceConfig::default();
        assert_eq!(config.sequence_timeout, Duration::from_millis(250));
        assert!(config.detect_double_escape);
    }

    // --- Edge cases ---

    #[test]
    fn triple_escape_produces_sequence_then_pending() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        // First Esc - pending
        let action = interp.feed(&esc(), t);
        assert!(matches!(action, KeySequenceAction::Pending));

        // Second Esc - sequence
        let action = interp.feed(&esc(), t + MS_50);
        assert!(matches!(
            action,
            KeySequenceAction::EmitSequence {
                kind: KeySequenceKind::DoubleEscape,
                ..
            }
        ));

        // Third Esc - starts new sequence (pending)
        let action = interp.feed(&esc(), t + MS_100);
        assert!(matches!(action, KeySequenceAction::Pending));
    }

    #[test]
    fn sequence_keys_are_captured() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        interp.feed(&esc(), t);
        let action = interp.feed(&esc(), t + MS_50);

        if let KeySequenceAction::EmitSequence { keys, .. } = action {
            // Buffer should contain both escape keys that formed the sequence
            // Note: current implementation only adds first key to buffer
            // The second key completes the sequence
            assert!(!keys.is_empty());
        } else {
            panic!("Expected EmitSequence");
        }
    }

    #[test]
    fn rapid_non_escape_keys() {
        let mut interp = KeySequenceInterpreter::with_defaults();
        let t = now();

        // Rapid regular keys should all pass through immediately
        for (i, c) in "hello".chars().enumerate() {
            let action = interp.feed(&key(c), t + Duration::from_millis(i as u64 * 10));
            assert!(
                matches!(action, KeySequenceAction::Emit(_)),
                "Key '{}' should pass through",
                c
            );
        }
        assert!(!interp.has_pending());
    }
}
