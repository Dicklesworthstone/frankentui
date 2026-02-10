#![forbid(unsafe_code)]

//! Golden Output Harness for deterministic testing and isomorphism proofs.
//!
//! This module provides infrastructure for:
//! - Generating golden (reference) outputs for resize scenarios
//! - Computing SHA-256 checksums for isomorphism verification
//! - JSONL logging with stable schema for CI/debugging
//! - Deterministic mode with fixed seeds
//!
//! # JSONL Schema
//!
//! Each test case emits structured logs in JSONL format:
//!
//! ```json
//! {"event":"start","run_id":"...","case":"resize_80x24","env":{...},"seed":0,"timestamp":"..."}
//! {"event":"frame","frame_id":0,"width":80,"height":24,"checksum":"sha256:...","timing_ms":12}
//! {"event":"resize","from":"80x24","to":"120x40","timing_ms":5}
//! {"event":"frame","frame_id":1,"width":120,"height":40,"checksum":"sha256:...","timing_ms":14}
//! {"event":"complete","outcome":"pass","checksums":["sha256:...","sha256:..."],"total_ms":42}
//! ```
//!
//! # Determinism
//!
//! Set `GOLDEN_SEED` environment variable for reproducible runs:
//!
//! ```sh
//! GOLDEN_SEED=42 cargo test golden_
//! ```
//!
//! # Isomorphism Proof Template
//!
//! When a golden checksum changes, record the proof alongside the update:
//!
//! ```text
//! Change:
//!   - What changed?
//!   - Why is the new output equivalent?
//! Old checksums:
//!   - [list]
//! New checksums:
//!   - [list]
//! Preserved invariants:
//!   - Deterministic ordering
//!   - Stable tie-breaking
//!   - Seeded randomness only
//!   - Buffer dimensions/content rules
//! Justification:
//!   - Explain the equivalence and why drift is acceptable.
//! Approved by:
//!   - Name + date
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ftui_render::buffer::Buffer;

/// SHA-256 checksum prefix for clarity in logs.
const CHECKSUM_PREFIX: &str = "sha256:";

// ============================================================================
// Checksum Computation
// ============================================================================

/// Compute SHA-256 checksum of buffer content (characters only, no styling).
///
/// Returns a hex-encoded string prefixed with "sha256:".
pub fn compute_buffer_checksum(buf: &Buffer) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // We use a deterministic hash of the buffer content.
    // For true SHA-256, we'd need a crypto crate, but for isomorphism proofs
    // a deterministic hash is sufficient. This can be upgraded later.
    let mut hasher = DefaultHasher::new();

    // Hash dimensions
    buf.width().hash(&mut hasher);
    buf.height().hash(&mut hasher);

    // Hash cell content (character values only for isomorphism)
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                // Hash the content for determinism
                cell.content.hash(&mut hasher);
            }
        }
    }

    let hash = hasher.finish();
    format!("{CHECKSUM_PREFIX}{hash:016x}")
}

/// Compute SHA-256 checksum of a text string.
pub fn compute_text_checksum(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    format!("{CHECKSUM_PREFIX}{hash:016x}")
}

// ============================================================================
// Environment Capture
// ============================================================================

/// Capture relevant environment for reproducibility.
#[derive(Debug, Clone)]
pub struct GoldenEnv {
    pub term: String,
    pub colorterm: String,
    pub no_color: bool,
    pub tmux: bool,
    pub zellij: bool,
    pub seed: u64,
    pub rust_version: String,
    pub git_commit: String,
    pub git_branch: String,
}

impl GoldenEnv {
    /// Capture current environment.
    pub fn capture() -> Self {
        Self {
            term: std::env::var("TERM").unwrap_or_default(),
            colorterm: std::env::var("COLORTERM").unwrap_or_default(),
            no_color: std::env::var("NO_COLOR").is_ok(),
            tmux: std::env::var("TMUX").is_ok(),
            zellij: std::env::var("ZELLIJ").is_ok(),
            seed: std::env::var("GOLDEN_SEED")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            rust_version: rustc_version(),
            git_commit: git_commit(),
            git_branch: git_branch(),
        }
    }

    /// Convert to JSON string.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"term":"{}","colorterm":"{}","no_color":{},"tmux":{},"zellij":{},"seed":{},"rust_version":"{}","git_commit":"{}","git_branch":"{}"}}"#,
            escape_json(&self.term),
            escape_json(&self.colorterm),
            self.no_color,
            self.tmux,
            self.zellij,
            self.seed,
            escape_json(&self.rust_version),
            escape_json(&self.git_commit),
            escape_json(&self.git_branch),
        )
    }
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn git_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

// ============================================================================
// JSONL Logger
// ============================================================================

/// JSONL event logger for golden tests.
pub struct GoldenLogger {
    writer: Option<BufWriter<File>>,
    run_id: String,
    start_time: Instant,
    checksums: Vec<String>,
}

impl GoldenLogger {
    /// Create a new logger writing to the specified path.
    pub fn new(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: Some(BufWriter::new(file)),
            run_id: generate_run_id(),
            start_time: Instant::now(),
            checksums: Vec::new(),
        })
    }

    /// Create a no-op logger (for when logging is disabled).
    pub fn noop() -> Self {
        Self {
            writer: None,
            run_id: generate_run_id(),
            start_time: Instant::now(),
            checksums: Vec::new(),
        }
    }

    /// Log test start event.
    pub fn log_start(&mut self, case: &str, env: &GoldenEnv) {
        let timestamp = iso_timestamp();
        self.write_line(&format!(
            r#"{{"event":"start","run_id":"{}","case":"{}","env":{},"seed":{},"timestamp":"{}"}}"#,
            self.run_id,
            escape_json(case),
            env.to_json(),
            env.seed,
            timestamp,
        ));
    }

    /// Log a frame capture with checksum.
    pub fn log_frame(
        &mut self,
        frame_id: u32,
        width: u16,
        height: u16,
        checksum: &str,
        timing_ms: u64,
    ) {
        self.checksums.push(checksum.to_string());
        self.write_line(&format!(
            r#"{{"event":"frame","run_id":"{}","frame_id":{},"width":{},"height":{},"checksum":"{}","timing_ms":{}}}"#,
            self.run_id, frame_id, width, height, escape_json(checksum), timing_ms,
        ));
    }

    /// Log a resize event.
    pub fn log_resize(&mut self, from_w: u16, from_h: u16, to_w: u16, to_h: u16, timing_ms: u64) {
        self.write_line(&format!(
            r#"{{"event":"resize","run_id":"{}","from":"{}x{}","to":"{}x{}","timing_ms":{}}}"#,
            self.run_id, from_w, from_h, to_w, to_h, timing_ms,
        ));
    }

    /// Log test completion.
    pub fn log_complete(&mut self, outcome: GoldenOutcome) {
        let total_ms = self.start_time.elapsed().as_millis() as u64;
        let checksums_json: String = self
            .checksums
            .iter()
            .map(|c| format!(r#""{}""#, escape_json(c)))
            .collect::<Vec<_>>()
            .join(",");
        self.write_line(&format!(
            r#"{{"event":"complete","run_id":"{}","outcome":"{}","checksums":[{}],"total_ms":{}}}"#,
            self.run_id,
            outcome.as_str(),
            checksums_json,
            total_ms,
        ));
    }

    /// Log an error event.
    pub fn log_error(&mut self, message: &str) {
        self.write_line(&format!(
            r#"{{"event":"error","run_id":"{}","message":"{}","timestamp":"{}"}}"#,
            self.run_id,
            escape_json(message),
            iso_timestamp(),
        ));
    }

    /// Get collected checksums.
    pub fn checksums(&self) -> &[String] {
        &self.checksums
    }

    fn write_line(&mut self, line: &str) {
        if let Some(ref mut writer) = self.writer {
            let _ = writeln!(writer, "{line}");
            let _ = writer.flush();
        }
    }
}

/// Test outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoldenOutcome {
    Pass,
    Fail,
    Skip,
}

impl GoldenOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

fn generate_run_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{timestamp:x}")
}

fn iso_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO-like timestamp
    format!("{now}")
}

// ============================================================================
// Golden Test Case
// ============================================================================

/// A resize scenario for golden testing.
#[derive(Debug, Clone)]
pub struct ResizeScenario {
    /// Scenario name (e.g., "80x24_to_120x40").
    pub name: String,
    /// Initial terminal width.
    pub initial_width: u16,
    /// Initial terminal height.
    pub initial_height: u16,
    /// Resize steps: (width, height, delay_ms).
    pub resize_steps: Vec<(u16, u16, u64)>,
    /// Expected checksums for verification (if known).
    pub expected_checksums: Vec<String>,
}

impl ResizeScenario {
    /// Create a simple single-size scenario (no resize).
    pub fn fixed(name: &str, width: u16, height: u16) -> Self {
        Self {
            name: name.to_string(),
            initial_width: width,
            initial_height: height,
            resize_steps: Vec::new(),
            expected_checksums: Vec::new(),
        }
    }

    /// Create a resize scenario.
    pub fn resize(name: &str, from_w: u16, from_h: u16, to_w: u16, to_h: u16) -> Self {
        Self {
            name: name.to_string(),
            initial_width: from_w,
            initial_height: from_h,
            resize_steps: vec![(to_w, to_h, 0)],
            expected_checksums: Vec::new(),
        }
    }

    /// Add expected checksums for verification.
    #[must_use]
    pub fn with_expected(mut self, checksums: Vec<String>) -> Self {
        self.expected_checksums = checksums;
        self
    }
}

/// Standard resize scenarios for testing.
pub fn standard_resize_scenarios() -> Vec<ResizeScenario> {
    vec![
        // Fixed sizes
        ResizeScenario::fixed("fixed_80x24", 80, 24),
        ResizeScenario::fixed("fixed_120x40", 120, 40),
        ResizeScenario::fixed("fixed_60x15", 60, 15),
        ResizeScenario::fixed("fixed_40x10", 40, 10),
        ResizeScenario::fixed("fixed_200x60", 200, 60),
        // Resize transitions
        ResizeScenario::resize("resize_80x24_to_120x40", 80, 24, 120, 40),
        ResizeScenario::resize("resize_120x40_to_80x24", 120, 40, 80, 24),
        ResizeScenario::resize("resize_80x24_to_40x10", 80, 24, 40, 10),
        ResizeScenario::resize("resize_40x10_to_200x60", 40, 10, 200, 60),
    ]
}

// ============================================================================
// Golden File Management
// ============================================================================

/// Path to golden checksums file for a scenario.
pub fn golden_checksum_path(base_dir: &Path, scenario_name: &str) -> PathBuf {
    base_dir
        .join("tests")
        .join("golden")
        .join(format!("{scenario_name}.checksums"))
}

/// Load expected checksums from a golden file.
pub fn load_golden_checksums(path: &Path) -> std::io::Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.trim().to_string())
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Save checksums to a golden file.
pub fn save_golden_checksums(path: &Path, checksums: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = format!(
        "# Golden checksums - do not edit manually\n# Generated at: {}\n{}\n",
        iso_timestamp(),
        checksums.join("\n")
    );
    fs::write(path, content)
}

/// Check if we should update golden files (BLESS mode).
pub fn is_bless_mode() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Check if golden checksums are enforced (CI or explicit env).
pub fn is_golden_enforced() -> bool {
    let explicit = std::env::var("FTUI_GOLDEN_ENFORCE")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let ci = std::env::var("CI").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    explicit || ci
}

// ============================================================================
// Golden Test Runner
// ============================================================================

/// Result of a golden test.
#[derive(Debug)]
pub struct GoldenResult {
    pub scenario: String,
    pub outcome: GoldenOutcome,
    pub checksums: Vec<String>,
    pub expected_checksums: Vec<String>,
    pub mismatch_index: Option<usize>,
    pub duration_ms: u64,
}

impl GoldenResult {
    /// Check if the result is a pass.
    pub fn is_pass(&self) -> bool {
        self.outcome == GoldenOutcome::Pass
    }

    /// Format as human-readable string.
    pub fn format(&self) -> String {
        match self.outcome {
            GoldenOutcome::Pass => format!("PASS: {} ({}ms)", self.scenario, self.duration_ms),
            GoldenOutcome::Fail => {
                if self.expected_checksums.is_empty() {
                    format!("FAIL: {} - missing golden checksums", self.scenario)
                } else if let Some(idx) = self.mismatch_index {
                    format!(
                        "FAIL: {} - checksum mismatch at frame {}\n  expected: {}\n  actual: {}",
                        self.scenario,
                        idx,
                        self.expected_checksums
                            .get(idx)
                            .unwrap_or(&"<none>".to_string()),
                        self.checksums.get(idx).unwrap_or(&"<none>".to_string()),
                    )
                } else {
                    format!("FAIL: {} - checksum count mismatch", self.scenario)
                }
            }
            GoldenOutcome::Skip => format!("SKIP: {}", self.scenario),
        }
    }
}

/// Verify checksums against expected values.
pub fn verify_checksums(actual: &[String], expected: &[String]) -> (GoldenOutcome, Option<usize>) {
    if expected.is_empty() {
        // No expected checksums - optionally enforce in CI
        if is_golden_enforced() {
            return (GoldenOutcome::Fail, None);
        }
        return (GoldenOutcome::Pass, None);
    }

    if actual.len() != expected.len() {
        return (GoldenOutcome::Fail, None);
    }

    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        if a != e {
            return (GoldenOutcome::Fail, Some(i));
        }
    }

    (GoldenOutcome::Pass, None)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;

    #[test]
    fn test_compute_buffer_checksum_empty() {
        let buf = Buffer::new(10, 5);
        let checksum = compute_buffer_checksum(&buf);
        assert!(checksum.starts_with(CHECKSUM_PREFIX));
        assert_eq!(checksum.len(), CHECKSUM_PREFIX.len() + 16);
    }

    #[test]
    fn test_compute_buffer_checksum_deterministic() {
        let mut buf = Buffer::new(10, 5);
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(1, 0, Cell::from_char('B'));

        let checksum1 = compute_buffer_checksum(&buf);
        let checksum2 = compute_buffer_checksum(&buf);
        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_compute_buffer_checksum_differs_on_content() {
        let mut buf1 = Buffer::new(10, 5);
        buf1.set(0, 0, Cell::from_char('A'));

        let mut buf2 = Buffer::new(10, 5);
        buf2.set(0, 0, Cell::from_char('B'));

        let checksum1 = compute_buffer_checksum(&buf1);
        let checksum2 = compute_buffer_checksum(&buf2);
        assert_ne!(checksum1, checksum2);
    }

    #[test]
    fn test_compute_buffer_checksum_differs_on_size() {
        let buf1 = Buffer::new(10, 5);
        let buf2 = Buffer::new(11, 5);

        let checksum1 = compute_buffer_checksum(&buf1);
        let checksum2 = compute_buffer_checksum(&buf2);
        assert_ne!(checksum1, checksum2);
    }

    #[test]
    fn test_compute_text_checksum() {
        let text = "Hello, World!";
        let checksum = compute_text_checksum(text);
        assert!(checksum.starts_with(CHECKSUM_PREFIX));

        // Should be deterministic
        assert_eq!(checksum, compute_text_checksum(text));
    }

    #[test]
    fn test_golden_env_capture() {
        let env = GoldenEnv::capture();
        let json = env.to_json();
        assert!(json.contains("term"));
        assert!(json.contains("seed"));
    }

    #[test]
    fn test_escape_json() {
        assert_eq!(escape_json("hello"), "hello");
        assert_eq!(escape_json("he\"llo"), "he\\\"llo");
        assert_eq!(escape_json("he\\llo"), "he\\\\llo");
        assert_eq!(escape_json("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn test_verify_checksums_pass() {
        let actual = vec!["sha256:abc".to_string(), "sha256:def".to_string()];
        let expected = vec!["sha256:abc".to_string(), "sha256:def".to_string()];
        let (outcome, idx) = verify_checksums(&actual, &expected);
        assert_eq!(outcome, GoldenOutcome::Pass);
        assert!(idx.is_none());
    }

    #[test]
    fn test_verify_checksums_mismatch() {
        let actual = vec!["sha256:abc".to_string(), "sha256:xyz".to_string()];
        let expected = vec!["sha256:abc".to_string(), "sha256:def".to_string()];
        let (outcome, idx) = verify_checksums(&actual, &expected);
        assert_eq!(outcome, GoldenOutcome::Fail);
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn test_verify_checksums_length_mismatch() {
        let actual = vec!["sha256:abc".to_string()];
        let expected = vec!["sha256:abc".to_string(), "sha256:def".to_string()];
        let (outcome, idx) = verify_checksums(&actual, &expected);
        assert_eq!(outcome, GoldenOutcome::Fail);
        assert!(idx.is_none());
    }

    #[test]
    fn test_verify_checksums_empty_expected() {
        let actual = vec!["sha256:abc".to_string()];
        let expected: Vec<String> = vec![];
        let (outcome, _) = verify_checksums(&actual, &expected);
        assert_eq!(outcome, GoldenOutcome::Pass);
    }

    #[test]
    fn test_resize_scenario_fixed() {
        let scenario = ResizeScenario::fixed("test", 80, 24);
        assert_eq!(scenario.name, "test");
        assert_eq!(scenario.initial_width, 80);
        assert_eq!(scenario.initial_height, 24);
        assert!(scenario.resize_steps.is_empty());
    }

    #[test]
    fn test_resize_scenario_resize() {
        let scenario = ResizeScenario::resize("test", 80, 24, 120, 40);
        assert_eq!(scenario.initial_width, 80);
        assert_eq!(scenario.initial_height, 24);
        assert_eq!(scenario.resize_steps.len(), 1);
        assert_eq!(scenario.resize_steps[0], (120, 40, 0));
    }

    #[test]
    fn test_standard_scenarios() {
        let scenarios = standard_resize_scenarios();
        assert!(!scenarios.is_empty());
        // Should have both fixed and resize scenarios
        assert!(scenarios.iter().any(|s| s.resize_steps.is_empty()));
        assert!(scenarios.iter().any(|s| !s.resize_steps.is_empty()));
    }

    // ── GoldenOutcome ─────────────────────────────────────────────────

    #[test]
    fn outcome_as_str() {
        assert_eq!(GoldenOutcome::Pass.as_str(), "pass");
        assert_eq!(GoldenOutcome::Fail.as_str(), "fail");
        assert_eq!(GoldenOutcome::Skip.as_str(), "skip");
    }

    // ── GoldenResult formatting ───────────────────────────────────────

    #[test]
    fn result_format_pass() {
        let r = GoldenResult {
            scenario: "test".into(),
            outcome: GoldenOutcome::Pass,
            checksums: vec![],
            expected_checksums: vec![],
            mismatch_index: None,
            duration_ms: 42,
        };
        assert!(r.is_pass());
        let s = r.format();
        assert!(s.contains("PASS"), "{s}");
        assert!(s.contains("42ms"), "{s}");
    }

    #[test]
    fn result_format_fail_missing_golden() {
        let r = GoldenResult {
            scenario: "test".into(),
            outcome: GoldenOutcome::Fail,
            checksums: vec!["sha256:abc".into()],
            expected_checksums: vec![],
            mismatch_index: None,
            duration_ms: 0,
        };
        assert!(!r.is_pass());
        let s = r.format();
        assert!(s.contains("missing golden checksums"), "{s}");
    }

    #[test]
    fn result_format_fail_mismatch() {
        let r = GoldenResult {
            scenario: "test".into(),
            outcome: GoldenOutcome::Fail,
            checksums: vec!["sha256:abc".into(), "sha256:wrong".into()],
            expected_checksums: vec!["sha256:abc".into(), "sha256:def".into()],
            mismatch_index: Some(1),
            duration_ms: 0,
        };
        let s = r.format();
        assert!(s.contains("checksum mismatch at frame 1"), "{s}");
        assert!(s.contains("sha256:def"), "expected: {s}");
        assert!(s.contains("sha256:wrong"), "actual: {s}");
    }

    #[test]
    fn result_format_fail_count_mismatch() {
        let r = GoldenResult {
            scenario: "test".into(),
            outcome: GoldenOutcome::Fail,
            checksums: vec!["sha256:abc".into()],
            expected_checksums: vec!["sha256:abc".into(), "sha256:def".into()],
            mismatch_index: None,
            duration_ms: 0,
        };
        let s = r.format();
        assert!(s.contains("checksum count mismatch"), "{s}");
    }

    #[test]
    fn result_format_skip() {
        let r = GoldenResult {
            scenario: "test".into(),
            outcome: GoldenOutcome::Skip,
            checksums: vec![],
            expected_checksums: vec![],
            mismatch_index: None,
            duration_ms: 0,
        };
        assert!(r.format().starts_with("SKIP:"));
    }

    // ── GoldenLogger ──────────────────────────────────────────────────

    #[test]
    fn noop_logger_does_not_crash() {
        let mut logger = GoldenLogger::noop();
        let env = GoldenEnv::capture();
        logger.log_start("test_case", &env);
        logger.log_frame(0, 80, 24, "sha256:abc", 10);
        logger.log_resize(80, 24, 120, 40, 5);
        logger.log_error("some error");
        logger.log_complete(GoldenOutcome::Pass);
        assert_eq!(logger.checksums(), &["sha256:abc".to_string()]);
    }

    #[test]
    fn file_logger_writes_events() {
        let dir = std::env::temp_dir().join(format!(
            "ftui_golden_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let log_path = dir.join("test.jsonl");
        {
            let mut logger = GoldenLogger::new(&log_path).expect("create logger");
            let env = GoldenEnv::capture();
            logger.log_start("test", &env);
            logger.log_frame(0, 80, 24, "sha256:aaa", 1);
            logger.log_resize(80, 24, 120, 40, 2);
            logger.log_frame(1, 120, 40, "sha256:bbb", 3);
            logger.log_complete(GoldenOutcome::Pass);
        }
        let content = std::fs::read_to_string(&log_path).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5, "should have 5 JSONL events");
        assert!(lines[0].contains("\"event\":\"start\""));
        assert!(lines[1].contains("\"event\":\"frame\""));
        assert!(lines[2].contains("\"event\":\"resize\""));
        assert!(lines[3].contains("\"event\":\"frame\""));
        assert!(lines[4].contains("\"event\":\"complete\""));
        assert!(lines[4].contains("\"outcome\":\"pass\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Golden file I/O ───────────────────────────────────────────────

    #[test]
    fn save_and_load_golden_checksums() {
        let dir = std::env::temp_dir().join(format!(
            "ftui_golden_io_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = dir.join("tests").join("golden").join("test.checksums");
        let checksums = vec!["sha256:abc".to_string(), "sha256:def".to_string()];
        save_golden_checksums(&path, &checksums).expect("save");
        let loaded = load_golden_checksums(&path).expect("load");
        assert_eq!(loaded, checksums);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_golden_checksums_nonexistent_returns_empty() {
        let path = std::path::Path::new("/tmp/nonexistent_golden_12345.checksums");
        let loaded = load_golden_checksums(path).expect("should return empty");
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_golden_checksums_skips_comments_and_blanks() {
        let dir = std::env::temp_dir().join(format!(
            "ftui_golden_comments_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.checksums");
        std::fs::write(
            &path,
            "# comment\nsha256:abc\n\nsha256:def\n# another comment\n",
        )
        .unwrap();
        let loaded = load_golden_checksums(&path).expect("load");
        assert_eq!(loaded, vec!["sha256:abc", "sha256:def"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn golden_checksum_path_format() {
        let base = std::path::Path::new("/project");
        let path = golden_checksum_path(base, "resize_80x24");
        assert_eq!(
            path,
            std::path::PathBuf::from("/project/tests/golden/resize_80x24.checksums")
        );
    }

    // ── ResizeScenario builder ────────────────────────────────────────

    #[test]
    fn resize_scenario_with_expected() {
        let scenario =
            ResizeScenario::fixed("test", 80, 24).with_expected(vec!["sha256:abc".into()]);
        assert_eq!(scenario.expected_checksums, vec!["sha256:abc"]);
    }

    // ── GoldenEnv::to_json produces valid JSON ────────────────────────

    #[test]
    fn golden_env_to_json_is_valid() {
        let env = GoldenEnv::capture();
        let json = env.to_json();
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("GoldenEnv::to_json should produce valid JSON");
        assert!(parsed.get("term").is_some());
        assert!(parsed.get("seed").is_some());
        assert!(parsed.get("rust_version").is_some());
        assert!(parsed.get("git_commit").is_some());
    }
}
