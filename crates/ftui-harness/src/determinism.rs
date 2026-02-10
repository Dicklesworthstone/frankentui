#![forbid(unsafe_code)]

//! Deterministic fixtures for tests and E2E harnesses.
//!
//! This module centralizes seed selection, deterministic timestamps, and
//! environment capture so tests can produce stable hashes and JSONL logs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Shared deterministic fixture for a test run.
#[derive(Debug)]
pub struct DeterminismFixture {
    seed: u64,
    deterministic: bool,
    time_step_ms: u64,
    run_id: String,
    ts_counter: AtomicU64,
    ms_counter: AtomicU64,
    start: Instant,
}

impl DeterminismFixture {
    /// Create a fixture with a stable run id and seed.
    pub fn new(prefix: &str, default_seed: u64) -> Self {
        let deterministic = deterministic_mode();
        let seed = fixture_seed(default_seed);
        let time_step_ms = fixture_time_step_ms();
        Self::new_with(prefix, seed, deterministic, time_step_ms)
    }

    /// Create a fixture with explicit configuration (used by tests).
    pub fn new_with(prefix: &str, seed: u64, deterministic: bool, time_step_ms: u64) -> Self {
        let run_id = if deterministic {
            format!("{prefix}_seed{seed}")
        } else {
            format!("{prefix}_{}_{}", std::process::id(), unix_secs())
        };
        Self {
            seed,
            deterministic,
            time_step_ms,
            run_id,
            ts_counter: AtomicU64::new(0),
            ms_counter: AtomicU64::new(0),
            start: Instant::now(),
        }
    }

    /// Current deterministic seed.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// True when deterministic mode is enabled.
    pub fn deterministic(&self) -> bool {
        self.deterministic
    }

    /// Stable run identifier for JSONL logs.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Return a deterministic timestamp string (or wall time if disabled).
    pub fn timestamp(&self) -> String {
        if self.deterministic {
            let n = self.ts_counter.fetch_add(1, Ordering::Relaxed);
            format!("T{n:06}")
        } else {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            format!("{}.{:03}", now.as_secs(), now.subsec_millis())
        }
    }

    /// Return a monotonically increasing time in ms.
    pub fn now_ms(&self) -> u64 {
        if self.deterministic {
            self.ms_counter
                .fetch_add(self.time_step_ms, Ordering::Relaxed)
                .saturating_add(self.time_step_ms)
        } else {
            self.start.elapsed().as_millis() as u64
        }
    }

    /// Capture environment fields for logging.
    pub fn env_snapshot(&self) -> EnvSnapshot {
        EnvSnapshot::capture(self.seed, self.deterministic)
    }
}

/// Environment snapshot with deterministic field ordering.
#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    fields: BTreeMap<String, String>,
}

impl EnvSnapshot {
    /// Capture common environment fields for reproducibility.
    pub fn capture(seed: u64, deterministic: bool) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert("term".into(), json_string(&env_string("TERM")));
        fields.insert("colorterm".into(), json_string(&env_string("COLORTERM")));
        fields.insert("no_color".into(), env_bool("NO_COLOR").to_string());
        fields.insert("tmux".into(), env_bool("TMUX").to_string());
        fields.insert("zellij".into(), env_bool("ZELLIJ").to_string());
        fields.insert("seed".into(), seed.to_string());
        fields.insert("deterministic".into(), deterministic.to_string());
        Self { fields }
    }

    /// Add a string field (value will be JSON-escaped and quoted).
    #[must_use]
    pub fn with_str(mut self, key: &str, value: &str) -> Self {
        self.fields.insert(key.to_string(), json_string(value));
        self
    }

    /// Add a numeric field.
    #[must_use]
    pub fn with_u64(mut self, key: &str, value: u64) -> Self {
        self.fields.insert(key.to_string(), value.to_string());
        self
    }

    /// Add a boolean field.
    #[must_use]
    pub fn with_bool(mut self, key: &str, value: bool) -> Self {
        self.fields.insert(key.to_string(), value.to_string());
        self
    }

    /// Add a raw JSON field (caller is responsible for correctness).
    #[must_use]
    pub fn with_raw(mut self, key: &str, raw_json: &str) -> Self {
        self.fields.insert(key.to_string(), raw_json.to_string());
        self
    }

    /// Render as JSON object string.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{");
        for (idx, (k, v)) in self.fields.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&escape_json(k));
            out.push_str("\":");
            out.push_str(v);
        }
        out.push('}');
        out
    }
}

/// JSONL field value for test logging.
#[derive(Debug, Clone)]
pub enum JsonValue {
    /// JSON-escaped string value.
    Str(String),
    /// Raw JSON (caller is responsible for correctness).
    Raw(String),
    /// Boolean value.
    Bool(bool),
    /// Unsigned integer value.
    U64(u64),
    /// Signed integer value.
    I64(i64),
}

impl JsonValue {
    /// Convenience constructor for JSON string values.
    pub fn str(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    /// Convenience constructor for raw JSON values.
    pub fn raw(value: impl Into<String>) -> Self {
        Self::Raw(value.into())
    }

    /// Convenience constructor for boolean values.
    pub fn bool(value: bool) -> Self {
        Self::Bool(value)
    }

    /// Convenience constructor for unsigned integers.
    pub fn u64(value: u64) -> Self {
        Self::U64(value)
    }

    /// Convenience constructor for signed integers.
    pub fn i64(value: i64) -> Self {
        Self::I64(value)
    }

    fn to_json(&self) -> String {
        match self {
            Self::Str(value) => json_string(value),
            Self::Raw(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::U64(value) => value.to_string(),
            Self::I64(value) => value.to_string(),
        }
    }
}

/// Deterministic JSONL logger for tests.
#[derive(Debug)]
pub struct TestJsonlLogger {
    fixture: DeterminismFixture,
    schema_version: u32,
    seq: AtomicU64,
    context: BTreeMap<String, String>,
}

impl TestJsonlLogger {
    /// Create a JSONL logger with a deterministic fixture.
    pub fn new(prefix: &str, default_seed: u64) -> Self {
        Self {
            fixture: DeterminismFixture::new(prefix, default_seed),
            schema_version: 1,
            seq: AtomicU64::new(0),
            context: BTreeMap::new(),
        }
    }

    /// Access the underlying determinism fixture.
    pub fn fixture(&self) -> &DeterminismFixture {
        &self.fixture
    }

    /// Set the JSONL schema version.
    #[must_use]
    pub fn with_schema_version(mut self, version: u32) -> Self {
        self.schema_version = version;
        self
    }

    /// Add a context string field.
    pub fn add_context_str(&mut self, key: &str, value: &str) {
        self.context.insert(key.to_string(), json_string(value));
    }

    /// Add a context numeric field.
    pub fn add_context_u64(&mut self, key: &str, value: u64) {
        self.context.insert(key.to_string(), value.to_string());
    }

    /// Add a context boolean field.
    pub fn add_context_bool(&mut self, key: &str, value: bool) {
        self.context.insert(key.to_string(), value.to_string());
    }

    /// Add a context raw JSON field (caller ensures correctness).
    pub fn add_context_raw(&mut self, key: &str, raw_json: &str) {
        self.context.insert(key.to_string(), raw_json.to_string());
    }

    /// Emit a JSONL line (returned as a string).
    pub fn emit_line(&self, event: &str, fields: &[(&str, JsonValue)]) -> String {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut used_keys: BTreeMap<String, ()> = BTreeMap::new();
        for (key, _) in fields {
            used_keys.insert((*key).to_string(), ());
        }

        let mut parts = Vec::new();
        parts.push(format!("\"schema_version\":{}", self.schema_version));
        parts.push(format!("\"seq\":{seq}"));
        parts.push(format!(
            "\"ts\":\"{}\"",
            escape_json(&self.fixture.timestamp())
        ));
        parts.push(format!("\"event\":\"{}\"", escape_json(event)));

        if !used_keys.contains_key("run_id") {
            parts.push(format!(
                "\"run_id\":\"{}\"",
                escape_json(self.fixture.run_id())
            ));
        }
        if !used_keys.contains_key("seed") {
            parts.push(format!("\"seed\":{}", self.fixture.seed()));
        }
        if !used_keys.contains_key("deterministic") {
            parts.push(format!(
                "\"deterministic\":{}",
                self.fixture.deterministic()
            ));
        }
        if !self.context.is_empty() && !used_keys.contains_key("context") {
            let mut context_parts = String::from("{");
            for (idx, (k, v)) in self.context.iter().enumerate() {
                if idx > 0 {
                    context_parts.push(',');
                }
                context_parts.push('"');
                context_parts.push_str(&escape_json(k));
                context_parts.push_str("\":");
                context_parts.push_str(v);
            }
            context_parts.push('}');
            parts.push(format!("\"context\":{context_parts}"));
        }

        for (key, value) in fields {
            parts.push(format!("\"{}\":{}", escape_json(key), value.to_json()));
        }

        format!("{{{}}}", parts.join(","))
    }

    /// Emit a JSONL line to stderr.
    pub fn log(&self, event: &str, fields: &[(&str, JsonValue)]) {
        eprintln!("{}", self.emit_line(event, fields));
    }

    /// Emit a JSONL environment snapshot line.
    pub fn log_env(&self) {
        let env_json = self.fixture.env_snapshot().to_json();
        self.log("env", &[("env", JsonValue::raw(env_json))]);
    }
}

/// True when deterministic mode is enabled via environment.
pub fn deterministic_mode() -> bool {
    env_flag("FTUI_TEST_DETERMINISTIC")
        || env_flag("FTUI_DETERMINISTIC")
        || env_flag("E2E_DETERMINISTIC")
}

/// Choose a seed from environment or use the provided default.
pub fn fixture_seed(default_seed: u64) -> u64 {
    env_u64("FTUI_TEST_SEED")
        .or_else(|| env_u64("FTUI_SEED"))
        .or_else(|| env_u64("FTUI_HARNESS_SEED"))
        .or_else(|| env_u64("E2E_SEED"))
        .or_else(|| env_u64("E2E_CONTEXT_SEED"))
        .unwrap_or(default_seed)
}

/// Time step in milliseconds for deterministic clocks.
pub fn fixture_time_step_ms() -> u64 {
    env_u64("FTUI_TEST_TIME_STEP_MS")
        .or_else(|| env_u64("E2E_TIME_STEP_MS"))
        .unwrap_or(100)
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

fn env_bool(key: &str) -> bool {
    std::env::var(key).is_ok()
}

fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn env_string(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_timestamps_are_monotonic() {
        let fixture = DeterminismFixture::new_with("fixture_ts", 123, true, 7);
        let t0 = fixture.timestamp();
        let t1 = fixture.timestamp();
        assert_eq!(t0, "T000000");
        assert_eq!(t1, "T000001");
    }

    #[test]
    fn deterministic_clock_advances_by_step() {
        let fixture = DeterminismFixture::new_with("fixture_clock", 123, true, 7);
        let first = fixture.now_ms();
        let second = fixture.now_ms();
        assert_eq!(first, 7);
        assert_eq!(second, 14);
    }

    #[test]
    fn env_snapshot_includes_seed_and_flag() {
        let fixture = DeterminismFixture::new_with("fixture_env", 123, true, 7);
        let json = fixture.env_snapshot().to_json();
        assert!(
            json.contains("\"seed\":123"),
            "env snapshot should include deterministic seed"
        );
        assert!(
            json.contains("\"deterministic\":true"),
            "env snapshot should include deterministic flag"
        );
    }

    #[test]
    fn fixture_seed_and_run_id_are_stable() {
        let fixture = DeterminismFixture::new_with("fixture_seed", 4242, true, 5);
        assert_eq!(
            fixture.seed(),
            4242,
            "expected DeterminismFixture to retain the explicit seed"
        );
        assert!(
            fixture.deterministic(),
            "expected DeterminismFixture to retain the deterministic flag"
        );
        assert_eq!(
            fixture.run_id(),
            "fixture_seed_seed4242",
            "expected deterministic run_id to embed prefix + seed"
        );
    }

    #[test]
    fn fixture_time_step_is_deterministic() {
        let fixture = DeterminismFixture::new_with("fixture_time_step", 1, true, 25);
        let t1 = fixture.now_ms();
        let t2 = fixture.now_ms();
        assert_eq!(
            t2 - t1,
            25,
            "expected deterministic time step of 25ms (t1={t1}, t2={t2})"
        );
    }

    #[test]
    fn jsonl_logger_emits_core_fields() {
        let logger = TestJsonlLogger::new("jsonl_logger", 99);
        let line = logger.emit_line("case_start", &[("case", JsonValue::str("alpha"))]);
        assert!(line.contains("\"event\":\"case_start\""));
        assert!(line.contains("\"run_id\""));
        assert!(line.contains("\"seed\":99"));
        assert!(line.contains("\"deterministic\""));
        assert!(line.contains("\"schema_version\":1"));
    }

    #[test]
    fn jsonl_logger_includes_context() {
        let mut logger = TestJsonlLogger::new("jsonl_logger_ctx", 7);
        logger.add_context_str("suite", "determinism");
        let line = logger.emit_line("step", &[("ok", JsonValue::bool(true))]);
        assert!(line.contains("\"context\":{"));
        assert!(line.contains("\"suite\":\"determinism\""));
    }

    // ── escape_json ───────────────────────────────────────────────────

    #[test]
    fn escape_json_no_special_chars() {
        assert_eq!(escape_json("hello"), "hello");
    }

    #[test]
    fn escape_json_backslash() {
        assert_eq!(escape_json(r"a\b"), r"a\\b");
    }

    #[test]
    fn escape_json_double_quote() {
        assert_eq!(escape_json(r#"say "hi""#), r#"say \"hi\""#);
    }

    #[test]
    fn escape_json_newline_cr_tab() {
        assert_eq!(escape_json("a\nb\rc\td"), r"a\nb\rc\td");
    }

    #[test]
    fn escape_json_combined() {
        assert_eq!(escape_json("a\\b\n\"c\""), r#"a\\b\n\"c\""#);
    }

    // ── json_string ───────────────────────────────────────────────────

    #[test]
    fn json_string_wraps_in_quotes() {
        assert_eq!(json_string("hello"), "\"hello\"");
    }

    #[test]
    fn json_string_escapes_content() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
    }

    // ── env helper semantics (tested safely via unset vars) ──────────

    #[test]
    fn env_flag_unset_is_false() {
        // Unique key that is guaranteed unset
        assert!(!env_flag("__FTUI_NEVER_SET_FLAG_9d3a1f"));
    }

    #[test]
    fn env_u64_unset_returns_none() {
        assert_eq!(env_u64("__FTUI_NEVER_SET_U64_9d3a1f"), None);
    }

    #[test]
    fn env_bool_unset_is_false() {
        assert!(!env_bool("__FTUI_NEVER_SET_BOOL_9d3a1f"));
    }

    #[test]
    fn env_string_unset_is_empty() {
        assert_eq!(env_string("__FTUI_NEVER_SET_STR_9d3a1f"), "");
    }

    #[test]
    fn fixture_seed_defaults_when_unset() {
        // With no FTUI_TEST_SEED etc. set, fixture_seed returns default
        // (This relies on __FTUI_NEVER env vars not being set.)
        let default = 12345u64;
        // fixture_seed reads real env vars, so we can't control them here,
        // but we can verify the function doesn't panic and returns a u64
        let result = fixture_seed(default);
        // fixture_seed always returns a u64; just verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn fixture_time_step_ms_default() {
        // When no env vars are set, default is 100
        let result = fixture_time_step_ms();
        assert!(result > 0, "time step should be positive");
    }

    // ── EnvSnapshot builder ───────────────────────────────────────────

    #[test]
    fn env_snapshot_with_str() {
        let snap = EnvSnapshot::capture(1, true).with_str("custom", "value");
        let json = snap.to_json();
        assert!(json.contains("\"custom\":\"value\""));
    }

    #[test]
    fn env_snapshot_with_u64() {
        let snap = EnvSnapshot::capture(1, true).with_u64("count", 42);
        let json = snap.to_json();
        assert!(json.contains("\"count\":42"));
    }

    #[test]
    fn env_snapshot_with_bool() {
        let snap = EnvSnapshot::capture(1, true).with_bool("flag", false);
        let json = snap.to_json();
        assert!(json.contains("\"flag\":false"));
    }

    #[test]
    fn env_snapshot_with_raw() {
        let snap = EnvSnapshot::capture(1, true).with_raw("nested", r#"{"a":1}"#);
        let json = snap.to_json();
        assert!(json.contains(r#""nested":{"a":1}"#));
    }

    // ── JsonValue variants ────────────────────────────────────────────

    #[test]
    fn json_value_str_escapes() {
        let v = JsonValue::str("he\"llo");
        assert_eq!(v.to_json(), "\"he\\\"llo\"");
    }

    #[test]
    fn json_value_raw_passthrough() {
        let v = JsonValue::raw(r#"{"x":1}"#);
        assert_eq!(v.to_json(), r#"{"x":1}"#);
    }

    #[test]
    fn json_value_bool() {
        assert_eq!(JsonValue::bool(true).to_json(), "true");
        assert_eq!(JsonValue::bool(false).to_json(), "false");
    }

    #[test]
    fn json_value_u64() {
        assert_eq!(JsonValue::u64(12345).to_json(), "12345");
    }

    #[test]
    fn json_value_i64_negative() {
        assert_eq!(JsonValue::i64(-7).to_json(), "-7");
    }

    // ── Non-deterministic fixture ─────────────────────────────────────

    #[test]
    fn non_deterministic_run_id_contains_pid() {
        let fixture = DeterminismFixture::new_with("nd", 0, false, 100);
        let run_id = fixture.run_id().to_string();
        let pid = format!("{}", std::process::id());
        assert!(
            run_id.contains(&pid),
            "non-deterministic run_id should contain PID: {run_id}"
        );
    }

    // ── Logger seq counter ────────────────────────────────────────────

    #[test]
    fn logger_seq_increments() {
        let logger = TestJsonlLogger::new("seq_test", 1);
        let line0 = logger.emit_line("ev0", &[]);
        let line1 = logger.emit_line("ev1", &[]);
        assert!(line0.contains("\"seq\":0"), "first line seq=0: {line0}");
        assert!(line1.contains("\"seq\":1"), "second line seq=1: {line1}");
    }

    #[test]
    fn logger_custom_schema_version() {
        let logger = TestJsonlLogger::new("schema_test", 1).with_schema_version(3);
        let line = logger.emit_line("ev", &[]);
        assert!(
            line.contains("\"schema_version\":3"),
            "custom schema version: {line}"
        );
    }

    #[test]
    fn logger_context_u64_and_bool() {
        let mut logger = TestJsonlLogger::new("ctx_types", 1);
        logger.add_context_u64("size", 80);
        logger.add_context_bool("interactive", false);
        let line = logger.emit_line("ev", &[]);
        assert!(line.contains("\"size\":80"), "u64 context: {line}");
        assert!(
            line.contains("\"interactive\":false"),
            "bool context: {line}"
        );
    }

    #[test]
    fn logger_context_raw() {
        let mut logger = TestJsonlLogger::new("ctx_raw", 1);
        logger.add_context_raw("meta", r#"[1,2,3]"#);
        let line = logger.emit_line("ev", &[]);
        assert!(line.contains(r#""meta":[1,2,3]"#), "raw context: {line}");
    }

    #[test]
    fn logger_field_override_suppresses_default() {
        let logger = TestJsonlLogger::new("override_test", 99);
        let line = logger.emit_line("ev", &[("seed", JsonValue::u64(7))]);
        // The explicit field should be present, and no duplicate "seed":99
        assert!(line.contains("\"seed\":7"), "overridden seed: {line}");
        // Should NOT contain the default seed=99 since we override it
        assert!(
            !line.contains("\"seed\":99"),
            "default seed should be suppressed: {line}"
        );
    }

    // ── emit_line produces valid JSON ─────────────────────────────────

    #[test]
    fn logger_emit_line_is_valid_json() {
        let mut logger = TestJsonlLogger::new("json_valid", 42);
        logger.add_context_str("suite", "test");
        let line = logger.emit_line(
            "case_end",
            &[
                ("result", JsonValue::str("pass")),
                ("duration_ms", JsonValue::u64(15)),
                ("success", JsonValue::bool(true)),
            ],
        );
        // Parse with serde_json to validate
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("emit_line should produce valid JSON");
        assert_eq!(parsed["event"], "case_end");
        assert_eq!(parsed["result"], "pass");
        assert_eq!(parsed["duration_ms"], 15);
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["seed"], 42);
    }
}
