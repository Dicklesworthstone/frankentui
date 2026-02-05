#![forbid(unsafe_code)]

//! Shared JSONL logging helpers for tests.

use std::sync::atomic::{AtomicU64, Ordering};

/// Schema version for test JSONL logs.
pub const TEST_JSONL_SCHEMA: &str = "test-jsonl-v1";

/// Returns true if JSONL logging should be emitted.
#[must_use]
pub fn jsonl_enabled() -> bool {
    std::env::var("E2E_JSONL").is_ok() || std::env::var("CI").is_ok()
}

/// Escape a string for JSON output (minimal string escaping).
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
            _ => out.push(ch),
        }
    }
    out
}

/// JSONL logger with stable run context + per-entry sequence numbering.
pub struct JsonlLogger {
    run_id: String,
    seed: Option<u64>,
    context: Vec<(String, String)>,
    seq: AtomicU64,
}

impl JsonlLogger {
    /// Create a new JSONL logger with a run identifier.
    #[must_use]
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            seed: None,
            context: Vec::new(),
            seq: AtomicU64::new(0),
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

    /// Emit a JSONL line if logging is enabled.
    pub fn log(&self, event: &str, fields: &[(&str, &str)]) {
        if !jsonl_enabled() {
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

        eprintln!("{{{}}}", parts.join(","));
    }
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
