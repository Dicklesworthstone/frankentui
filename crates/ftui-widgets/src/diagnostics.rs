#![forbid(unsafe_code)]

//! Reusable diagnostic logging and telemetry substrate.
//!
//! This module provides the shared infrastructure for JSONL diagnostic
//! logging and telemetry hooks, extracted from the common patterns in
//! [`crate::inspector`] and the demo showcase's `mouse_playground`.
//!
//! # Design
//!
//! The core types are generic over the entry type, so each consumer
//! defines its own `DiagnosticEntry` / `DiagnosticEventKind` while
//! reusing the log, dispatch, and checksum infrastructure.
//!
//! # Key Types
//!
//! - `DiagnosticRecord` — trait for entries that can be serialized to JSONL
//! - `DiagnosticLog` — bounded in-memory log with optional stderr mirroring
//! - `TelemetryCallback` — type alias for observer callbacks
//! - `fnv1a_hash` — FNV-1a checksum utility for determinism verification
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::diagnostics::{DiagnosticLog, DiagnosticRecord};
//!
//! #[derive(Debug, Clone)]
//! struct MyEntry { kind: &'static str, data: u64 }
//!
//! impl DiagnosticRecord for MyEntry {
//!     fn to_jsonl(&self) -> String {
//!         format!("{{\"kind\":\"{}\",\"data\":{}}}", self.kind, self.data)
//!     }
//! }
//!
//! let mut log = DiagnosticLog::<MyEntry>::new();
//! log.record(MyEntry { kind: "test", data: 42 });
//! assert_eq!(log.entries().len(), 1);
//! ```

use std::fmt;
use std::io::Write;

// =============================================================================
// DiagnosticRecord trait
// =============================================================================

/// Trait for diagnostic entries that can be serialized to JSONL.
///
/// Consumers define their own entry structs with domain-specific fields
/// and implement this trait to plug into [`DiagnosticLog`].
pub trait DiagnosticRecord: fmt::Debug + Clone {
    /// Format this entry as a single JSONL line (no trailing newline).
    fn to_jsonl(&self) -> String;
}

/// Trait implemented by telemetry hook collections that can observe
/// diagnostic entries of type `E`.
pub trait DiagnosticHookDispatch<E>: fmt::Debug {
    /// Dispatch a single diagnostic entry to any registered hooks.
    fn dispatch(&self, entry: &E);
}

/// Encode a string as a JSON string literal.
///
/// The returned value includes the surrounding quotes and correctly escapes
/// control characters so the result can be embedded directly into JSONL output.
#[must_use]
pub fn json_string_literal(value: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if c < '\u{20}' => {
                let _ = write!(&mut out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// =============================================================================
// DiagnosticLog<E>
// =============================================================================

/// Whether stderr is a terminal, checked once per process.
fn stderr_is_terminal() -> bool {
    static IS_TERMINAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_TERMINAL.get_or_init(|| std::io::IsTerminal::is_terminal(&std::io::stderr()))
}

/// Bounded in-memory diagnostic log with optional stderr mirroring.
///
/// Generic over the entry type `E` so different subsystems can use
/// their own entry structs while sharing the log infrastructure.
#[derive(Debug)]
pub struct DiagnosticLog<E: DiagnosticRecord> {
    /// Collected entries.
    entries: Vec<E>,
    /// Logical start index after bounded evictions.
    head: usize,
    /// Maximum entries to keep (0 = unlimited).
    max_entries: usize,
    /// Whether to also write to stderr.
    write_stderr: bool,
}

impl<E: DiagnosticRecord> Default for DiagnosticLog<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: DiagnosticRecord> DiagnosticLog<E> {
    /// Create a new diagnostic log with a default capacity of 10 000 entries.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            head: 0,
            max_entries: 10_000,
            write_stderr: false,
        }
    }

    /// Enable stderr mirroring — each recorded entry is also written
    /// to stderr as a JSONL line, unless stderr is a terminal: that is the
    /// terminal the UI draws on, and a line written there lands between
    /// frames (bd-g00-root-epic-ewths.33.4). Redirect stderr to capture it.
    #[must_use]
    pub fn with_stderr(mut self) -> Self {
        self.write_stderr = true;
        self
    }

    /// Set the maximum number of entries to keep. When the log is full,
    /// the oldest entry is evicted. Pass `0` for unlimited.
    #[must_use]
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max;
        self
    }

    /// Record a diagnostic entry.
    pub fn record(&mut self, entry: E) {
        if self.write_stderr && !stderr_is_terminal() {
            let _ = writeln!(std::io::stderr(), "{}", entry.to_jsonl());
        }

        self.entries.push(entry);
        if self.max_entries > 0 && self.entries.len().saturating_sub(self.head) > self.max_entries {
            self.head += 1;
            if self.head >= self.entries.len() / 2 {
                self.entries = self.entries.split_off(self.head);
                self.head = 0;
            }
        }
    }

    /// Get all entries.
    pub fn entries(&self) -> &[E] {
        &self.entries[self.head..]
    }

    /// Get entries matching a predicate.
    pub fn entries_matching(&self, predicate: impl Fn(&E) -> bool) -> Vec<&E> {
        self.entries().iter().filter(|e| predicate(e)).collect()
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.head = 0;
    }

    /// Export all entries as a JSONL string (newline-separated).
    pub fn to_jsonl(&self) -> String {
        self.entries()
            .iter()
            .map(DiagnosticRecord::to_jsonl)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.len().saturating_sub(self.head)
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// =============================================================================
// DiagnosticSupport<E, H>
// =============================================================================

/// Shared state for optional diagnostic logging plus optional telemetry hooks.
///
/// This is the reusable control-flow skeleton shared by diagnostic-enabled
/// widgets and screens:
///
/// - optional bounded log
/// - optional hook collection
/// - shared `record()` ordering: hooks first, then log
pub struct DiagnosticSupport<E: DiagnosticRecord, H: DiagnosticHookDispatch<E>> {
    log: Option<DiagnosticLog<E>>,
    hooks: Option<H>,
}

impl<E: DiagnosticRecord, H: DiagnosticHookDispatch<E>> Default for DiagnosticSupport<E, H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: DiagnosticRecord, H: DiagnosticHookDispatch<E>> fmt::Debug for DiagnosticSupport<E, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticSupport")
            .field("log", &self.log)
            .field("hooks", &self.hooks)
            .finish()
    }
}

impl<E: DiagnosticRecord, H: DiagnosticHookDispatch<E>> DiagnosticSupport<E, H> {
    /// Create an empty diagnostic support bundle with no log and no hooks.
    pub fn new() -> Self {
        Self {
            log: None,
            hooks: None,
        }
    }

    /// Enable logging with the provided diagnostic log.
    #[must_use]
    pub fn with_log(mut self, log: DiagnosticLog<E>) -> Self {
        self.log = Some(log);
        self
    }

    /// Enable telemetry hooks with the provided hook set.
    #[must_use]
    pub fn with_hooks(mut self, hooks: H) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Replace the diagnostic log.
    pub fn set_log(&mut self, log: DiagnosticLog<E>) {
        self.log = Some(log);
    }

    /// Replace the telemetry hooks.
    pub fn set_hooks(&mut self, hooks: H) {
        self.hooks = Some(hooks);
    }

    /// Borrow the diagnostic log, if enabled.
    pub fn log(&self) -> Option<&DiagnosticLog<E>> {
        self.log.as_ref()
    }

    /// Mutably borrow the diagnostic log, if enabled.
    pub fn log_mut(&mut self) -> Option<&mut DiagnosticLog<E>> {
        self.log.as_mut()
    }

    /// Borrow the telemetry hooks, if enabled.
    pub fn hooks(&self) -> Option<&H> {
        self.hooks.as_ref()
    }

    /// Returns true when either logging or hooks are enabled.
    pub fn is_active(&self) -> bool {
        self.log.is_some() || self.hooks.is_some()
    }

    /// Dispatch an entry to hooks first, then record it to the log.
    pub fn record(&mut self, entry: E) {
        if let Some(ref hooks) = self.hooks {
            hooks.dispatch(&entry);
        }
        if let Some(ref mut log) = self.log {
            log.record(entry);
        }
    }
}

// =============================================================================
// TelemetryCallback type alias
// =============================================================================

/// Callback type for telemetry hooks.
///
/// Generic over the entry type so each subsystem can observe its own
/// domain-specific entries.
pub type TelemetryCallback<E> = Box<dyn Fn(&E) + Send + Sync>;

// =============================================================================
// FNV-1a checksum utility
// =============================================================================

/// Compute an FNV-1a 64-bit hash of the given byte slice.
///
/// This is the same algorithm used by both `inspector` and
/// `mouse_playground` for determinism verification checksums.
pub fn fnv1a_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

// =============================================================================
// Helpers for environment-flag-based diagnostics
// =============================================================================

/// Check an environment variable as a boolean diagnostic flag.
///
/// Returns `true` if the variable is set to `"1"` or `"true"` (case-insensitive).
pub fn env_flag_enabled(var_name: &str) -> bool {
    std::env::var(var_name)
        .map(|v| env_flag_value_enabled(&v))
        .unwrap_or(false)
}

fn env_flag_value_enabled(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct TestEntry {
        kind: &'static str,
        value: u64,
    }

    impl DiagnosticRecord for TestEntry {
        fn to_jsonl(&self) -> String {
            format!("{{\"kind\":\"{}\",\"value\":{}}}", self.kind, self.value)
        }
    }

    #[test]
    fn log_records_and_retrieves() {
        let mut log = DiagnosticLog::<TestEntry>::new();
        log.record(TestEntry {
            kind: "a",
            value: 1,
        });
        log.record(TestEntry {
            kind: "b",
            value: 2,
        });
        assert_eq!(log.len(), 2);
        assert_eq!(log.entries()[0].value, 1);
        assert_eq!(log.entries()[1].value, 2);
    }

    #[test]
    fn log_evicts_oldest_when_full() {
        let mut log = DiagnosticLog::<TestEntry>::new().with_max_entries(2);
        log.record(TestEntry {
            kind: "a",
            value: 1,
        });
        log.record(TestEntry {
            kind: "b",
            value: 2,
        });
        log.record(TestEntry {
            kind: "c",
            value: 3,
        });
        assert_eq!(log.len(), 2);
        assert_eq!(log.entries()[0].value, 2);
        assert_eq!(log.entries()[1].value, 3);
    }

    #[test]
    fn log_preserves_order_after_many_evictions() {
        let mut log = DiagnosticLog::<TestEntry>::new().with_max_entries(3);
        for value in 0..16 {
            log.record(TestEntry { kind: "x", value });
        }
        let values: Vec<u64> = log.entries().iter().map(|entry| entry.value).collect();
        assert_eq!(values, vec![13, 14, 15]);
    }

    #[test]
    fn log_clear() {
        let mut log = DiagnosticLog::<TestEntry>::new();
        log.record(TestEntry {
            kind: "a",
            value: 1,
        });
        assert!(!log.is_empty());
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn log_to_jsonl() {
        let mut log = DiagnosticLog::<TestEntry>::new();
        log.record(TestEntry {
            kind: "x",
            value: 10,
        });
        log.record(TestEntry {
            kind: "y",
            value: 20,
        });
        let output = log.to_jsonl();
        assert!(output.contains("\"kind\":\"x\""));
        assert!(output.contains("\"kind\":\"y\""));
        assert!(output.contains('\n'));
    }

    #[test]
    fn log_entries_matching() {
        let mut log = DiagnosticLog::<TestEntry>::new();
        log.record(TestEntry {
            kind: "a",
            value: 1,
        });
        log.record(TestEntry {
            kind: "b",
            value: 2,
        });
        log.record(TestEntry {
            kind: "a",
            value: 3,
        });
        let matches = log.entries_matching(|e| e.kind == "a");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn json_string_literal_escapes_control_characters() {
        let escaped = json_string_literal("line 1\nline\t2");
        assert_eq!(escaped, "\"line 1\\nline\\t2\"");
    }

    #[test]
    fn fnv1a_hash_deterministic() {
        let h1 = fnv1a_hash(b"hello world");
        let h2 = fnv1a_hash(b"hello world");
        assert_eq!(h1, h2);
        assert_ne!(h1, fnv1a_hash(b"hello worlD"));
    }

    #[test]
    fn fnv1a_hash_empty() {
        let h = fnv1a_hash(b"");
        assert_eq!(h, 0xcbf29ce484222325); // FNV offset basis
    }

    #[test]
    fn env_flag_enabled_false_when_unset() {
        // Use a unique variable name unlikely to be set
        assert!(!env_flag_enabled("FTUI_TEST_DIAGNOSTICS_NEVER_SET_12345"));
    }

    #[test]
    fn env_flag_enabled_accepts_true_case_insensitively() {
        assert!(env_flag_value_enabled("TrUe"));
    }

    #[test]
    fn env_flag_enabled_accepts_one() {
        assert!(env_flag_value_enabled("1"));
    }

    #[test]
    fn default_log_has_correct_capacity() {
        let log = DiagnosticLog::<TestEntry>::new();
        assert_eq!(log.max_entries, 10_000);
        assert!(!log.write_stderr);
    }
}
