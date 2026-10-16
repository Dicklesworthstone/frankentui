#![forbid(unsafe_code)]

//! Shared JSONL logging helpers for the demo showcase.
//!
//! This module is the **authoritative** owner of JSONL event logging across the
//! demo-showcase crate. It provides:
//!
//! - A single [`escape_json`] implementation (handles all control chars).
//! - [`JsonlLogger`] — a thread-safe, sequenced event logger with builder API.
//! - `LoggerFactory` — a reusable bootstrap pattern (env gating, `OnceLock`,
//!   run_id / seed / screen_mode) so each subsystem doesn't re-invent the wheel.
//! - [`JsonlSink`] — stderr or file sink abstraction.
//!
//! **All JSONL emitters inside `ftui-demo-showcase` should go through this module.**

use std::fs::{OpenOptions, create_dir_all};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::determinism;

/// Schema version for test JSONL logs.
pub const TEST_JSONL_SCHEMA: &str = "test-jsonl-v1";

/// Returns true if JSONL logging should be emitted.
#[must_use]
pub fn jsonl_enabled() -> bool {
    std::env::var("E2E_JSONL").is_ok() || std::env::var("CI").is_ok()
}

/// Returns true if the named env var is set to `"1"` or `"true"` (case-insensitive).
#[must_use]
pub fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// JSON string escaping
// ---------------------------------------------------------------------------

/// Escape a string for JSON output.
///
/// Handles `"`, `\\`, `\n`, `\r`, `\t`, and all other control characters
/// (`U+0000`..`U+001F`) via `\uXXXX` notation.
#[must_use]
pub fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Sink abstraction
// ---------------------------------------------------------------------------

/// Where a [`JsonlLogger`] writes its output.
#[derive(Debug, Clone)]
pub enum JsonlSink {
    /// Write to stderr (diagnostic channel).
    Stderr,
    /// Append to a file at the given path (creates parent dirs as needed).
    File(String),
}

impl JsonlSink {
    /// Best-effort write of a complete JSONL line (including trailing newline).
    pub fn write_line(&self, line: &str) {
        match self {
            Self::Stderr => {
                let _ = writeln!(std::io::stderr(), "{line}");
            }
            Self::File(path) => {
                if let Some(parent) = Path::new(path).parent()
                    && !parent.as_os_str().is_empty()
                {
                    let _ = create_dir_all(parent);
                }
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(file, "{line}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// JSONL field value (string vs numeric)
// ---------------------------------------------------------------------------

/// A value in a JSONL field — either a quoted string or a raw numeric/boolean literal.
#[derive(Debug, Clone)]
pub enum JsonlValue<'a> {
    /// A string value that will be JSON-escaped and quoted.
    Str(&'a str),
    /// A raw literal emitted as-is (for numbers, booleans, null).
    Raw(&'a str),
}

// ---------------------------------------------------------------------------
// JsonlLogger
// ---------------------------------------------------------------------------

/// JSONL logger with stable run context + per-entry sequence numbering.
pub struct JsonlLogger {
    run_id: String,
    seed: Option<u64>,
    context: Vec<(String, String)>,
    seq: AtomicU64,
    sink: JsonlSink,
    /// When `Some`, this function is checked instead of [`jsonl_enabled`].
    gate: Option<fn() -> bool>,
}

impl JsonlLogger {
    /// Create a new JSONL logger with a run identifier (defaults to stderr sink).
    #[must_use]
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            seed: None,
            context: Vec::new(),
            seq: AtomicU64::new(0),
            sink: JsonlSink::Stderr,
            gate: None,
        }
    }

    /// Attach a deterministic seed field to all log entries.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Add a context field to all log entries.
    #[must_use]
    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.push((key.into(), value.into()));
        self
    }

    /// Set the output sink (default: stderr).
    #[must_use]
    pub fn with_sink(mut self, sink: JsonlSink) -> Self {
        self.sink = sink;
        self
    }

    /// Override the enable-gate function (default: [`jsonl_enabled`]).
    #[must_use]
    pub fn with_gate(mut self, gate: fn() -> bool) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Returns `true` when this logger's gate is open.
    fn is_enabled(&self) -> bool {
        match self.gate {
            Some(gate) => gate(),
            None => jsonl_enabled(),
        }
    }

    /// Emit a JSONL line if logging is enabled (string-only fields).
    pub fn log(&self, event: &str, fields: &[(&str, &str)]) {
        if !self.is_enabled() {
            return;
        }

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut parts = Vec::with_capacity(6 + self.context.len() + fields.len());
        parts.push(format!("\"schema_version\":\"{}\"", TEST_JSONL_SCHEMA));
        parts.push(format!("\"run_id\":\"{}\"", escape_json(&self.run_id)));
        parts.push(format!("\"seq\":{seq}"));
        parts.push(format!("\"event\":\"{}\"", escape_json(event)));
        if let Some(seed) = self.seed {
            parts.push(format!("\"seed\":{seed}"));
        }
        for (key, value) in &self.context {
            parts.push(format!("\"{}\":\"{}\"", key, escape_json(value)));
        }
        for (key, value) in fields {
            parts.push(format!("\"{}\":\"{}\"", key, escape_json(value)));
        }

        let line = format!("{{{}}}", parts.join(","));
        self.sink.write_line(&line);
    }

    /// Emit a JSONL line with mixed string and raw/numeric fields.
    ///
    /// Each field is either `JsonlValue::Str` (escaped + quoted) or
    /// `JsonlValue::Raw` (emitted verbatim — for numbers, bools, null).
    pub fn log_mixed(&self, event: &str, fields: &[(&str, JsonlValue<'_>)]) {
        if !self.is_enabled() {
            return;
        }

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut parts = Vec::with_capacity(6 + self.context.len() + fields.len());
        parts.push(format!("\"schema_version\":\"{}\"", TEST_JSONL_SCHEMA));
        parts.push(format!("\"run_id\":\"{}\"", escape_json(&self.run_id)));
        parts.push(format!("\"seq\":{seq}"));
        parts.push(format!("\"event\":\"{}\"", escape_json(event)));
        if let Some(seed) = self.seed {
            parts.push(format!("\"seed\":{seed}"));
        }
        for (key, value) in &self.context {
            parts.push(format!("\"{}\":\"{}\"", key, escape_json(value)));
        }
        for (key, value) in fields {
            match value {
                JsonlValue::Str(s) => {
                    parts.push(format!("\"{}\":\"{}\"", key, escape_json(s)));
                }
                JsonlValue::Raw(r) => {
                    parts.push(format!("\"{}\":{}", key, r));
                }
            }
        }

        let line = format!("{{{}}}", parts.join(","));
        self.sink.write_line(&line);
    }

    /// Return the current sequence counter value (for testing / diagnostics).
    pub fn seq_value(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Allocate and return the next sequence number without emitting a line.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Logger factory — reusable bootstrap pattern
// ---------------------------------------------------------------------------

/// Create a [`JsonlLogger`] pre-configured with the standard demo context fields
/// (run_id from env, seed, screen_mode).
///
/// This is the factory function that replaces the copy-pasted `OnceLock` + env-gate
/// pattern used by `screen_init_logger`, `mouse_jsonl_logger`, `perf_challenge_logger`, etc.
///
/// # Arguments
/// * `default_run_id` — fallback run-id if `FTUI_DEMO_RUN_ID` is unset.
/// * `extra_context`  — additional key-value pairs to attach to every entry.
/// * `sink`           — where to write (stderr or file).
/// * `gate`           — optional enable-gate override.
#[must_use]
pub fn make_demo_logger(
    default_run_id: &str,
    extra_context: &[(&str, &str)],
    sink: JsonlSink,
    gate: Option<fn() -> bool>,
) -> JsonlLogger {
    let run_id = determinism::demo_run_id().unwrap_or_else(|| default_run_id.to_string());
    let seed = determinism::demo_seed(0);
    let mut logger = JsonlLogger::new(run_id)
        .with_seed(seed)
        .with_context("screen_mode", determinism::demo_screen_mode())
        .with_sink(sink);
    if let Some(gate) = gate {
        logger = logger.with_gate(gate);
    }
    for &(key, value) in extra_context {
        logger = logger.with_context(key, value);
    }
    logger
}

/// Convenience wrapper: create a demo logger behind a `OnceLock`, returning
/// `None` when the gate is closed.
///
/// This is the exact pattern that was copy-pasted across `screen_init_logger`,
/// `mouse_jsonl_logger`, `perf_challenge_logger`. Now it's a single function.
pub fn demo_logger(
    cell: &'static OnceLock<JsonlLogger>,
    default_run_id: &str,
    extra_context: &[(&str, &str)],
) -> Option<&'static JsonlLogger> {
    if !jsonl_enabled() {
        return None;
    }
    Some(
        cell.get_or_init(|| {
            make_demo_logger(default_run_id, extra_context, JsonlSink::Stderr, None)
        }),
    )
}

/// Emit a key dispatch JSONL line when `FTUI_DEMO_KEY_JSONL` is set to a non-empty path
/// (or `"stderr"` / `"-"`).
pub fn emit_key_dispatch_jsonl(outcome: &str, chord: &str, action: &str) {
    let Ok(path) = std::env::var("FTUI_DEMO_KEY_JSONL") else {
        return;
    };
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    let sink = if path == "-" || path.eq_ignore_ascii_case("stderr") {
        JsonlSink::Stderr
    } else {
        JsonlSink::File(path.to_string())
    };
    let escaped_action = escape_json(action);
    let line = format!(
        "{{\"event\":\"key_dispatch\",\"outcome\":\"{}\",\"chord\":\"{}\",\"action\":\"{}\",\"binding\":\"{}\"}}",
        escape_json(outcome),
        escape_json(chord),
        escaped_action,
        escaped_action,
    );
    sink.write_line(&line);
}

/// Validate the Mermaid mega showcase recompute JSONL schema.
pub fn validate_mega_recompute_jsonl_schema(line: &str) -> Result<(), String> {
    let required_fields = [
        "\"schema_version\":",
        "\"event\":\"mermaid_mega_recompute\"",
        "\"seq\":",
        "\"timestamp\":",
        "\"seed\":",
        "\"screen_mode\":",
        "\"sample\":",
        "\"diagram_type\":",
        "\"layout_mode\":",
        "\"tier\":",
        "\"glyph_mode\":",
        "\"wrap_mode\":",
        "\"render_mode\":",
        "\"palette\":",
        "\"styles_enabled\":",
        "\"comparison_enabled\":",
        "\"comparison_layout_mode\":",
        "\"viewport_cols\":",
        "\"viewport_rows\":",
        "\"render_cols\":",
        "\"render_rows\":",
        "\"zoom\":",
        "\"pan_x\":",
        "\"pan_y\":",
        "\"analysis_epoch\":",
        "\"layout_epoch\":",
        "\"render_epoch\":",
        "\"analysis_ran\":",
        "\"layout_ran\":",
        "\"render_ran\":",
        "\"cache_hits\":",
        "\"cache_misses\":",
        "\"cache_hit\":",
        "\"debounce_skips\":",
        "\"layout_budget_exceeded\":",
        "\"parse_ms\":",
        "\"layout_ms\":",
        "\"render_ms\":",
        "\"node_count\":",
        "\"edge_count\":",
        "\"error_count\":",
        "\"layout_iterations\":",
        "\"layout_iterations_max\":",
        "\"layout_budget_exceeded_layout\":",
        "\"layout_crossings\":",
        "\"layout_ranks\":",
        "\"layout_max_rank_width\":",
        "\"layout_total_bends\":",
        "\"layout_position_variance\":",
    ];

    for field in required_fields {
        if !line.contains(field) {
            return Err(format!(
                "mega recompute JSONL missing required field {field}: {line}"
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape_json tests ────────────────────────────────────────────

    #[test]
    fn escape_json_no_special_chars() {
        assert_eq!(escape_json("hello world"), "hello world");
    }

    #[test]
    fn escape_json_quotes() {
        assert_eq!(escape_json(r#"say "hi""#), r#"say \"hi\""#);
    }

    #[test]
    fn escape_json_backslash() {
        assert_eq!(escape_json(r"path\to\file"), r"path\\to\\file");
    }

    #[test]
    fn escape_json_newline_and_tab() {
        assert_eq!(escape_json("line1\nline2\ttab"), "line1\\nline2\\ttab");
    }

    #[test]
    fn escape_json_carriage_return() {
        assert_eq!(escape_json("a\rb"), "a\\rb");
    }

    #[test]
    fn escape_json_empty_string() {
        assert_eq!(escape_json(""), "");
    }

    #[test]
    fn escape_json_combined_special_chars() {
        let input = "He said \"hello\"\nThen \\left\r";
        let expected = "He said \\\"hello\\\"\\nThen \\\\left\\r";
        assert_eq!(escape_json(input), expected);
    }

    #[test]
    fn escape_json_unicode_passes_through() {
        assert_eq!(escape_json("🦀 café"), "🦀 café");
    }

    #[test]
    fn escape_json_control_chars() {
        // Control characters other than \n, \r, \t should be \uXXXX escaped.
        let s = "a\x01b\x7fc";
        let escaped = escape_json(s);
        assert!(
            escaped.contains("\\u0001"),
            "expected \\u0001 in: {escaped}"
        );
    }

    // ── JsonlLogger tests ────────────────────────────────────────────

    #[test]
    fn logger_new_creates_with_run_id() {
        let logger = JsonlLogger::new("test-run-42");
        assert_eq!(logger.run_id, "test-run-42");
        assert!(logger.seed.is_none());
        assert!(logger.context.is_empty());
    }

    #[test]
    fn logger_with_seed_sets_seed() {
        let logger = JsonlLogger::new("run").with_seed(12345);
        assert_eq!(logger.seed, Some(12345));
    }

    #[test]
    fn logger_with_context_adds_field() {
        let logger = JsonlLogger::new("run")
            .with_context("key1", "value1")
            .with_context("key2", "value2");
        assert_eq!(logger.context.len(), 2);
        assert_eq!(
            logger.context[0],
            ("key1".to_string(), "value1".to_string())
        );
        assert_eq!(
            logger.context[1],
            ("key2".to_string(), "value2".to_string())
        );
    }

    #[test]
    fn logger_seq_increments() {
        let logger = JsonlLogger::new("run");
        assert_eq!(logger.seq.load(Ordering::Relaxed), 0);
        // Calling log won't emit (E2E_JSONL/CI not set in test env), but seq
        // increments only when jsonl_enabled() is true, so we test the atomic directly.
        let seq = logger.seq.fetch_add(1, Ordering::Relaxed);
        assert_eq!(seq, 0);
        let seq = logger.seq.fetch_add(1, Ordering::Relaxed);
        assert_eq!(seq, 1);
    }

    #[test]
    fn logger_builder_chain() {
        // Verify the builder pattern compiles and chains correctly.
        let logger = JsonlLogger::new("chained")
            .with_seed(999)
            .with_context("env", "test");
        assert_eq!(logger.run_id, "chained");
        assert_eq!(logger.seed, Some(999));
        assert_eq!(logger.context.len(), 1);
    }

    #[test]
    fn logger_with_sink_file() {
        let logger = JsonlLogger::new("run").with_sink(JsonlSink::File("/tmp/test.jsonl".into()));
        // Just verifying the builder compiles and sink is set.
        assert!(matches!(logger.sink, JsonlSink::File(_)));
    }

    #[test]
    fn logger_with_gate() {
        fn always_off() -> bool {
            false
        }
        let logger = JsonlLogger::new("run").with_gate(always_off);
        assert!(!logger.is_enabled());
    }

    #[test]
    fn logger_next_seq_increments() {
        let logger = JsonlLogger::new("run");
        assert_eq!(logger.next_seq(), 0);
        assert_eq!(logger.next_seq(), 1);
        assert_eq!(logger.seq_value(), 2);
    }

    // ── validate_mega_recompute_jsonl_schema tests ───────────────────

    #[test]
    fn schema_validation_rejects_empty() {
        assert!(validate_mega_recompute_jsonl_schema("").is_err());
    }

    #[test]
    fn schema_validation_rejects_partial() {
        let partial = r#"{"schema_version":"test-jsonl-v1","event":"mermaid_mega_recompute"}"#;
        assert!(validate_mega_recompute_jsonl_schema(partial).is_err());
    }

    #[test]
    fn schema_validation_error_names_missing_field() {
        let err = validate_mega_recompute_jsonl_schema("{}").unwrap_err();
        assert!(
            err.contains("missing required field"),
            "error should name the missing field: {err}"
        );
    }

    #[test]
    fn schema_validation_accepts_complete_line() {
        // Build a JSONL line containing all required fields.
        let line = [
            r#""schema_version":"test-jsonl-v1""#,
            r#""event":"mermaid_mega_recompute""#,
            r#""seq":0"#,
            r#""timestamp":"2026-01-01T00:00:00Z""#,
            r#""seed":42"#,
            r#""screen_mode":"normal""#,
            r#""sample":0"#,
            r#""diagram_type":"flowchart""#,
            r#""layout_mode":"auto""#,
            r#""tier":"default""#,
            r#""glyph_mode":"unicode""#,
            r#""wrap_mode":"none""#,
            r#""render_mode":"full""#,
            r#""palette":"dark""#,
            r#""styles_enabled":true"#,
            r#""comparison_enabled":false"#,
            r#""comparison_layout_mode":"auto""#,
            r#""viewport_cols":80"#,
            r#""viewport_rows":24"#,
            r#""render_cols":80"#,
            r#""render_rows":24"#,
            r#""zoom":1.0"#,
            r#""pan_x":0"#,
            r#""pan_y":0"#,
            r#""analysis_epoch":0"#,
            r#""layout_epoch":0"#,
            r#""render_epoch":0"#,
            r#""analysis_ran":true"#,
            r#""layout_ran":true"#,
            r#""render_ran":true"#,
            r#""cache_hits":0"#,
            r#""cache_misses":0"#,
            r#""cache_hit":true"#,
            r#""debounce_skips":0"#,
            r#""layout_budget_exceeded":false"#,
            r#""parse_ms":0.1"#,
            r#""layout_ms":0.2"#,
            r#""render_ms":0.3"#,
            r#""node_count":5"#,
            r#""edge_count":4"#,
            r#""error_count":0"#,
            r#""layout_iterations":1"#,
            r#""layout_iterations_max":10"#,
            r#""layout_budget_exceeded_layout":false"#,
            r#""layout_crossings":0"#,
            r#""layout_ranks":2"#,
            r#""layout_max_rank_width":3"#,
            r#""layout_total_bends":0"#,
            r#""layout_position_variance":0.5"#,
        ]
        .join(",");
        let full = format!("{{{line}}}");
        assert!(
            validate_mega_recompute_jsonl_schema(&full).is_ok(),
            "complete line should validate"
        );
    }

    // ── TEST_JSONL_SCHEMA constant test ──────────────────────────────

    #[test]
    fn schema_version_constant_is_set() {
        assert!(!TEST_JSONL_SCHEMA.is_empty());
        assert_eq!(TEST_JSONL_SCHEMA, "test-jsonl-v1");
    }
}
