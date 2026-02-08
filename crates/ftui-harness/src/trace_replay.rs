#![forbid(unsafe_code)]

//! Render-trace replay harness.
//!
//! Replays render-trace v1 JSONL logs into a deterministic buffer model,
//! verifies per-frame checksums, and reports mismatches with clear diagnostics.
//!
//! Designed for CI use: non-interactive, bounded, and deterministic.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const VOI_BUCKETS: usize = 6;

#[derive(Debug, Clone)]
enum TraceContent {
    Empty,
    Char(u32),
    Grapheme(Vec<u8>),
    Continuation,
}

impl TraceContent {
    fn kind(&self) -> u8 {
        match self {
            Self::Empty => 0,
            Self::Char(_) => 1,
            Self::Grapheme(_) => 2,
            Self::Continuation => 3,
        }
    }
}

#[derive(Debug, Clone)]
struct TraceCell {
    content: TraceContent,
    fg: u32,
    bg: u32,
    attrs: u32,
}

impl Default for TraceCell {
    fn default() -> Self {
        Self {
            content: TraceContent::Empty,
            fg: ftui_render::cell::PackedRgba::WHITE.0,
            bg: ftui_render::cell::PackedRgba::TRANSPARENT.0,
            attrs: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct TraceGrid {
    width: u16,
    height: u16,
    cells: Vec<TraceCell>,
}

impl TraceGrid {
    fn new(width: u16, height: u16) -> Self {
        let len = width as usize * height as usize;
        Self {
            width,
            height,
            cells: vec![TraceCell::default(); len],
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        *self = Self::new(width, height);
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    fn set_cell(&mut self, x: u16, y: u16, cell: TraceCell) -> io::Result<()> {
        let idx = self
            .index(x, y)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cell out of bounds"))?;
        self.cells[idx] = cell;
        Ok(())
    }

    fn checksum(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        for cell in &self.cells {
            let kind = cell.content.kind();
            fnv1a_update(&mut hash, &[kind]);
            match &cell.content {
                TraceContent::Empty | TraceContent::Continuation => {
                    fnv1a_update(&mut hash, &0u16.to_le_bytes());
                }
                TraceContent::Char(codepoint) => {
                    let ch = char::from_u32(*codepoint).unwrap_or('\u{FFFD}');
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    let bytes = encoded.as_bytes();
                    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
                    fnv1a_update(&mut hash, &len.to_le_bytes());
                    fnv1a_update(&mut hash, &bytes[..len as usize]);
                }
                TraceContent::Grapheme(bytes) => {
                    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
                    fnv1a_update(&mut hash, &len.to_le_bytes());
                    fnv1a_update(&mut hash, &bytes[..len as usize]);
                }
            }
            fnv1a_update(&mut hash, &cell.fg.to_le_bytes());
            fnv1a_update(&mut hash, &cell.bg.to_le_bytes());
            fnv1a_update(&mut hash, &cell.attrs.to_le_bytes());
        }
        hash
    }

    fn apply_diff_runs(&mut self, payload: &[u8]) -> io::Result<ApplyStats> {
        let mut cursor = io::Cursor::new(payload);
        let width = read_u16(&mut cursor)?;
        let height = read_u16(&mut cursor)?;
        let run_count = read_u32(&mut cursor)? as usize;

        if width != self.width || height != self.height {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload dimensions do not match frame dimensions",
            ));
        }

        let mut cells_applied = 0usize;
        for _ in 0..run_count {
            let y = read_u16(&mut cursor)?;
            let x0 = read_u16(&mut cursor)?;
            let x1 = read_u16(&mut cursor)?;
            if x1 < x0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid run range",
                ));
            }
            if y >= self.height || x1 >= self.width {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "run out of bounds",
                ));
            }
            for x in x0..=x1 {
                let cell = read_cell(&mut cursor)?;
                self.set_cell(x, y, cell)?;
                cells_applied += 1;
            }
        }

        if cursor.position() as usize != payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload has trailing bytes",
            ));
        }

        Ok(ApplyStats {
            runs: run_count,
            cells: cells_applied,
        })
    }

    fn apply_full_buffer(&mut self, payload: &[u8]) -> io::Result<ApplyStats> {
        let mut cursor = io::Cursor::new(payload);
        let width = read_u16(&mut cursor)?;
        let height = read_u16(&mut cursor)?;
        if width != self.width || height != self.height {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload dimensions do not match frame dimensions",
            ));
        }

        let mut cells_applied = 0usize;
        for y in 0..height {
            for x in 0..width {
                let cell = read_cell(&mut cursor)?;
                self.set_cell(x, y, cell)?;
                cells_applied += 1;
            }
        }

        if cursor.position() as usize != payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload has trailing bytes",
            ));
        }

        Ok(ApplyStats {
            runs: height as usize,
            cells: cells_applied,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct ApplyStats {
    runs: usize,
    cells: usize,
}

/// Result summary for a replay run.
#[derive(Debug, Clone)]
pub struct ReplaySummary {
    pub frames: usize,
    pub last_checksum: Option<u64>,
}

/// Configuration for failing-trace minimization.
#[derive(Debug, Clone, Copy)]
pub struct MinimizeOptions {
    /// Hard cap on replay attempts.
    pub max_attempts: usize,
    /// Optional wall-clock budget.
    pub max_duration: Option<Duration>,
    /// Bayesian prior alpha for preservation probability.
    pub alpha_prior: f64,
    /// Bayesian prior beta for preservation probability.
    pub beta_prior: f64,
}

impl Default for MinimizeOptions {
    fn default() -> Self {
        Self {
            max_attempts: 2048,
            max_duration: Some(Duration::from_secs(30)),
            alpha_prior: 1.0,
            beta_prior: 3.0,
        }
    }
}

/// One minimization replay attempt (evidence ledger row).
#[derive(Debug, Clone)]
pub struct MinimizeAttempt {
    pub attempt: usize,
    pub granularity: usize,
    pub removed_lines: usize,
    pub candidate_lines: usize,
    pub replay_ms: u64,
    pub replay_class: String,
    pub preserved_failure: bool,
    pub posterior_mean: f64,
    pub posterior_variance: f64,
    pub voi_score: f64,
}

/// Final minimization output and ledger summary.
#[derive(Debug, Clone)]
pub struct MinimizeReport {
    pub input_path: PathBuf,
    pub output_path: PathBuf,
    pub baseline_error: String,
    pub baseline_class: String,
    pub final_error: String,
    pub original_lines: usize,
    pub minimized_lines: usize,
    pub attempts: usize,
    pub preserved_attempts: usize,
    pub duration_ms: u64,
    pub ledger: Vec<MinimizeAttempt>,
}

#[derive(Debug, Clone)]
struct ReplayFailure {
    class: String,
    message: String,
}

#[derive(Debug, Clone, Copy)]
struct BetaPosterior {
    alpha: f64,
    beta: f64,
}

impl BetaPosterior {
    fn new(alpha: f64, beta: f64) -> Self {
        Self { alpha, beta }
    }

    fn mean(self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    fn variance(self) -> f64 {
        (self.alpha * self.beta)
            / ((self.alpha + self.beta).powi(2) * (self.alpha + self.beta + 1.0))
    }

    fn update(&mut self, success: bool) {
        if success {
            self.alpha += 1.0;
        } else {
            self.beta += 1.0;
        }
    }
}

/// Minimize a failing trace to a smaller failing trace using ddmin + VOI ordering.
pub fn minimize_failing_trace(
    input_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
    options: MinimizeOptions,
) -> io::Result<MinimizeReport> {
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();
    let start = Instant::now();

    if options.max_attempts == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "max_attempts must be > 0",
        ));
    }
    if options.alpha_prior <= 0.0 || options.beta_prior <= 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "alpha_prior and beta_prior must be > 0",
        ));
    }

    let lines = read_trace_lines(input_path)?;
    if lines.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trace file is empty",
        ));
    }

    let baseline = replay_trace(input_path).map_or_else(
        |err| {
            let msg = err.to_string();
            Ok(ReplayFailure {
                class: classify_replay_error(&msg),
                message: msg,
            })
        },
        |_| {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "trace replay succeeded; minimizer requires a failing trace",
            ))
        },
    )?;

    let deadline = options.max_duration.map(|d| start + d);
    let mut keep: Vec<usize> = (0..lines.len()).collect();
    let mut granularity = keep.len().clamp(2, usize::MAX);
    let mut attempts = 0usize;
    let mut preserved_attempts = 0usize;
    let mut ledger = Vec::new();
    let mut posteriors = [BetaPosterior::new(1.0, 1.0); VOI_BUCKETS];
    for posterior in &mut posteriors {
        *posterior = BetaPosterior::new(options.alpha_prior, options.beta_prior);
    }

    let mut scratch_path = std::env::temp_dir();
    let scratch_id = format!(
        "ftui-trace-minimize-{}-{}.jsonl",
        std::process::id(),
        start.elapsed().as_nanos()
    );
    scratch_path.push(scratch_id);

    while keep.len() > 1 && attempts < options.max_attempts {
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            break;
        }

        let ranges = partition_ranges(keep.len(), granularity);
        let mut candidates = Vec::new();
        for (chunk_idx, (start_idx, end_idx)) in ranges.into_iter().enumerate() {
            let removed = end_idx.saturating_sub(start_idx);
            if removed == 0 || removed >= keep.len() {
                continue;
            }
            let bucket = removal_bucket(removed, keep.len());
            let posterior = posteriors[bucket];
            let score = voi_score(removed, keep.len().saturating_sub(removed), posterior);
            candidates.push((chunk_idx, start_idx, end_idx, removed, bucket, score));
        }
        if candidates.is_empty() {
            break;
        }
        candidates.sort_by(|a, b| {
            b.5.partial_cmp(&a.5)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let mut accepted: Option<Vec<usize>> = None;
        for (_chunk_idx, start_idx, end_idx, removed, bucket, score) in candidates {
            if attempts >= options.max_attempts {
                break;
            }
            if deadline.is_some_and(|limit| Instant::now() >= limit) {
                break;
            }

            let candidate_lines = keep.len() - removed;
            let mut candidate = Vec::with_capacity(candidate_lines);
            candidate.extend_from_slice(&keep[..start_idx]);
            candidate.extend_from_slice(&keep[end_idx..]);

            write_trace_subset(&lines, &candidate, &scratch_path)?;
            let replay_start = Instant::now();
            let (preserved, replay_class) = match replay_trace(&scratch_path) {
                Ok(_) => (false, "pass".to_string()),
                Err(err) => {
                    let msg = err.to_string();
                    let class = classify_replay_error(&msg);
                    (class == baseline.class, class)
                }
            };
            let replay_ms = replay_start.elapsed().as_millis() as u64;
            let posterior = posteriors[bucket];
            let attempt = MinimizeAttempt {
                attempt: attempts + 1,
                granularity,
                removed_lines: removed,
                candidate_lines,
                replay_ms,
                replay_class,
                preserved_failure: preserved,
                posterior_mean: posterior.mean(),
                posterior_variance: posterior.variance(),
                voi_score: score,
            };
            ledger.push(attempt);
            attempts += 1;
            posteriors[bucket].update(preserved);

            if preserved {
                preserved_attempts += 1;
                accepted = Some(candidate);
                break;
            }
        }

        if let Some(next_keep) = accepted {
            keep = next_keep;
            granularity = granularity.saturating_sub(1).max(2);
        } else if granularity >= keep.len() {
            break;
        } else {
            granularity = (granularity.saturating_mul(2)).min(keep.len());
        }
    }

    write_trace_subset(&lines, &keep, output_path)?;
    let final_err = replay_trace(output_path).map_or_else(
        |err| Ok(err.to_string()),
        |_| {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "minimized trace unexpectedly replays successfully",
            ))
        },
    )?;
    let final_class = classify_replay_error(&final_err);
    if final_class != baseline.class {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "minimized trace changed failure class: baseline={} minimized={}",
                baseline.class, final_class
            ),
        ));
    }

    Ok(MinimizeReport {
        input_path: input_path.to_path_buf(),
        output_path: output_path.to_path_buf(),
        baseline_error: baseline.message,
        baseline_class: baseline.class,
        final_error: final_err,
        original_lines: lines.len(),
        minimized_lines: keep.len(),
        attempts,
        preserved_attempts,
        duration_ms: start.elapsed().as_millis() as u64,
        ledger,
    })
}

/// Write a minimization report as a structured JSON document.
pub fn write_minimization_report_json(
    path: impl AsRef<Path>,
    report: &MinimizeReport,
) -> io::Result<()> {
    let mut attempts_json = Vec::with_capacity(report.ledger.len());
    for item in &report.ledger {
        attempts_json.push(serde_json::json!({
            "attempt": item.attempt,
            "granularity": item.granularity,
            "removed_lines": item.removed_lines,
            "candidate_lines": item.candidate_lines,
            "replay_ms": item.replay_ms,
            "replay_class": item.replay_class,
            "preserved_failure": item.preserved_failure,
            "posterior_mean": item.posterior_mean,
            "posterior_variance": item.posterior_variance,
            "voi_score": item.voi_score,
        }));
    }

    let payload = serde_json::json!({
        "input_path": report.input_path,
        "output_path": report.output_path,
        "baseline_error": report.baseline_error,
        "baseline_class": report.baseline_class,
        "final_error": report.final_error,
        "original_lines": report.original_lines,
        "minimized_lines": report.minimized_lines,
        "removed_lines": report.original_lines.saturating_sub(report.minimized_lines),
        "reduction_factor": if report.minimized_lines == 0 {
            serde_json::Value::Null
        } else {
            serde_json::Value::from(report.original_lines as f64 / report.minimized_lines as f64)
        },
        "attempts": report.attempts,
        "preserved_attempts": report.preserved_attempts,
        "duration_ms": report.duration_ms,
        "ledger": attempts_json,
    });

    let path = path.as_ref();
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, &payload)
        .map_err(|err| io::Error::other(format!("failed to serialize report: {err}")))?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Write the minimization evidence ledger as JSONL.
pub fn write_minimization_report_jsonl(
    path: impl AsRef<Path>,
    report: &MinimizeReport,
) -> io::Result<()> {
    let path = path.as_ref();
    let mut file = File::create(path)?;
    for item in &report.ledger {
        let line = serde_json::json!({
            "event": "minimize_attempt",
            "attempt": item.attempt,
            "granularity": item.granularity,
            "removed_lines": item.removed_lines,
            "candidate_lines": item.candidate_lines,
            "replay_ms": item.replay_ms,
            "replay_class": item.replay_class,
            "preserved_failure": item.preserved_failure,
            "posterior_mean": item.posterior_mean,
            "posterior_variance": item.posterior_variance,
            "voi_score": item.voi_score,
        });
        writeln!(file, "{line}")?;
    }
    Ok(())
}

/// Replay a render-trace JSONL file and verify per-frame checksums.
pub fn replay_trace(path: impl AsRef<Path>) -> io::Result<ReplaySummary> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let mut grid = TraceGrid::new(0, 0);
    let mut frames = 0usize;
    let mut last_checksum = None;

    for (line_idx, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid JSONL at line {}: {err}", line_idx + 1),
            )
        })?;
        let Some(event) = value.get("event").and_then(Value::as_str) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("missing event at line {}", line_idx + 1),
            ));
        };
        if event != "frame" {
            continue;
        }

        let frame_idx = parse_u64(&value, "frame_idx")?;
        let cols = parse_u16(&value, "cols")?;
        let rows = parse_u16(&value, "rows")?;
        let payload_kind = parse_str(&value, "payload_kind")?;
        let payload_path =
            parse_optional_str(&value, "payload_path").map(|p| resolve_payload_path(base_dir, &p));
        let expected_checksum = parse_hex_u64(parse_str(&value, "checksum")?)?;

        if grid.width != cols || grid.height != rows {
            grid.resize(cols, rows);
        }

        let stats = match payload_kind {
            "diff_runs_v1" => {
                let payload_path = payload_path.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "payload_path missing")
                })?;
                let payload = std::fs::read(&payload_path)?;
                grid.apply_diff_runs(&payload)?
            }
            "full_buffer_v1" => {
                let payload_path = payload_path.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "payload_path missing")
                })?;
                let payload = std::fs::read(&payload_path)?;
                grid.apply_full_buffer(&payload)?
            }
            "none" => ApplyStats { runs: 0, cells: 0 },
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported payload_kind {other} at frame {frame_idx}"),
                ));
            }
        };

        let actual_checksum = grid.checksum();
        if actual_checksum != expected_checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "checksum mismatch at frame {}: expected {:016x}, got {:016x} (payload_kind={}, runs={}, cells={})",
                    frame_idx,
                    expected_checksum,
                    actual_checksum,
                    payload_kind,
                    stats.runs,
                    stats.cells
                ),
            ));
        }

        frames += 1;
        last_checksum = Some(actual_checksum);
    }

    if frames == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no frame records found",
        ));
    }

    Ok(ReplaySummary {
        frames,
        last_checksum,
    })
}

fn classify_replay_error(message: &str) -> String {
    if message.contains("checksum mismatch") {
        "checksum_mismatch".to_string()
    } else if message.contains("no frame records found") {
        "no_frame_records".to_string()
    } else if message.contains("invalid JSONL") {
        "invalid_jsonl".to_string()
    } else if message.contains("payload dimensions do not match") {
        "dimension_mismatch".to_string()
    } else {
        "other".to_string()
    }
}

fn read_trace_lines(path: &Path) -> io::Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        lines.push(line?);
    }
    Ok(lines)
}

fn write_trace_subset(lines: &[String], keep: &[usize], path: &Path) -> io::Result<()> {
    let mut file = File::create(path)?;
    for &idx in keep {
        if let Some(line) = lines.get(idx) {
            writeln!(file, "{line}")?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "subset index out of bounds",
            ));
        }
    }
    Ok(())
}

fn partition_ranges(len: usize, parts: usize) -> Vec<(usize, usize)> {
    if len == 0 {
        return Vec::new();
    }
    let parts = parts.clamp(1, len);
    let base = len / parts;
    let extra = len % parts;
    let mut ranges = Vec::with_capacity(parts);
    let mut start = 0usize;
    for idx in 0..parts {
        let chunk = base + usize::from(idx < extra);
        let end = start + chunk;
        ranges.push((start, end));
        start = end;
    }
    ranges
}

fn removal_bucket(removed: usize, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let ratio = removed as f64 / total as f64;
    let bucket = (ratio * VOI_BUCKETS as f64).floor() as usize;
    bucket.min(VOI_BUCKETS - 1)
}

fn voi_score(removed: usize, candidate_lines: usize, posterior: BetaPosterior) -> f64 {
    let mean = posterior.mean();
    let variance = posterior.variance();
    let stddev = variance.sqrt();
    let expected_gain = removed as f64 * (mean + stddev);
    let replay_cost = candidate_lines.max(1) as f64;
    expected_gain / replay_cost
}

fn resolve_payload_path(base_dir: &Path, payload: &str) -> PathBuf {
    let payload_path = Path::new(payload);
    if payload_path.is_absolute() {
        payload_path.to_path_buf()
    } else {
        base_dir.join(payload_path)
    }
}

fn parse_u64(value: &Value, field: &str) -> io::Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {field}")))
}

fn parse_u16(value: &Value, field: &str) -> io::Result<u16> {
    let v = parse_u64(value, field)?;
    u16::try_from(v)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{field} out of range")))
}

fn parse_str<'a>(value: &'a Value, field: &str) -> io::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {field}")))
}

fn parse_optional_str(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn parse_hex_u64(value: &str) -> io::Result<u64> {
    let trimmed = value.trim().trim_start_matches("0x");
    if trimmed.len() != 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("checksum must be 16 hex chars, got {value}"),
        ));
    }
    u64::from_str_radix(trimmed, 16).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid checksum {value}: {err}"),
        )
    })
}

fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn read_u8<R: Read>(reader: &mut R) -> io::Result<u8> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u16<R: Read>(reader: &mut R) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_cell<R: Read>(reader: &mut R) -> io::Result<TraceCell> {
    let kind = read_u8(reader)?;
    let content = match kind {
        0 => TraceContent::Empty,
        1 => {
            let codepoint = read_u32(reader)?;
            if char::from_u32(codepoint).is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid char codepoint {codepoint}"),
                ));
            }
            TraceContent::Char(codepoint)
        }
        2 => {
            let len = read_u16(reader)? as usize;
            if len > 4096 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "grapheme length exceeds 4096",
                ));
            }
            let mut bytes = vec![0u8; len];
            reader.read_exact(&mut bytes)?;
            TraceContent::Grapheme(bytes)
        }
        3 => TraceContent::Continuation,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid content_kind {kind}"),
            ));
        }
    };
    let fg = read_u32(reader)?;
    let bg = read_u32(reader)?;
    let attrs = read_u32(reader)?;
    Ok(TraceCell {
        content,
        fg,
        bg,
        attrs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── FNV-1a hash ───────────────────────────────────────────────────

    #[test]
    fn fnv1a_empty_is_offset_basis() {
        let mut hash = FNV_OFFSET_BASIS;
        fnv1a_update(&mut hash, &[]);
        assert_eq!(hash, FNV_OFFSET_BASIS, "empty input should not change hash");
    }

    #[test]
    fn fnv1a_single_byte() {
        let mut hash = FNV_OFFSET_BASIS;
        fnv1a_update(&mut hash, &[0x61]); // 'a'
        // FNV-1a('a') = (offset_basis ^ 0x61) * prime
        let expected = (FNV_OFFSET_BASIS ^ 0x61).wrapping_mul(FNV_PRIME);
        assert_eq!(hash, expected);
    }

    #[test]
    fn fnv1a_deterministic() {
        let mut h1 = FNV_OFFSET_BASIS;
        let mut h2 = FNV_OFFSET_BASIS;
        let data = b"hello world";
        fnv1a_update(&mut h1, data);
        fnv1a_update(&mut h2, data);
        assert_eq!(h1, h2, "same input must yield same hash");
    }

    #[test]
    fn fnv1a_different_inputs_differ() {
        let mut h1 = FNV_OFFSET_BASIS;
        let mut h2 = FNV_OFFSET_BASIS;
        fnv1a_update(&mut h1, b"abc");
        fnv1a_update(&mut h2, b"abd");
        assert_ne!(h1, h2, "different inputs should yield different hashes");
    }

    // ── TraceContent::kind ────────────────────────────────────────────

    #[test]
    fn trace_content_kind_values() {
        assert_eq!(TraceContent::Empty.kind(), 0);
        assert_eq!(TraceContent::Char(65).kind(), 1);
        assert_eq!(TraceContent::Grapheme(vec![0xE2, 0x9A, 0x99]).kind(), 2);
        assert_eq!(TraceContent::Continuation.kind(), 3);
    }

    // ── TraceGrid ─────────────────────────────────────────────────────

    #[test]
    fn grid_new_correct_size() {
        let g = TraceGrid::new(3, 2);
        assert_eq!(g.width, 3);
        assert_eq!(g.height, 2);
        assert_eq!(g.cells.len(), 6);
    }

    #[test]
    fn grid_new_zero_dimensions() {
        let g = TraceGrid::new(0, 0);
        assert_eq!(g.cells.len(), 0);
    }

    #[test]
    fn grid_index_valid() {
        let g = TraceGrid::new(4, 3);
        assert_eq!(g.index(0, 0), Some(0));
        assert_eq!(g.index(3, 0), Some(3));
        assert_eq!(g.index(0, 1), Some(4));
        assert_eq!(g.index(3, 2), Some(11));
    }

    #[test]
    fn grid_index_out_of_bounds() {
        let g = TraceGrid::new(4, 3);
        assert_eq!(g.index(4, 0), None); // x == width
        assert_eq!(g.index(0, 3), None); // y == height
        assert_eq!(g.index(10, 10), None);
    }

    #[test]
    fn grid_set_cell_valid() {
        let mut g = TraceGrid::new(2, 2);
        let cell = TraceCell {
            content: TraceContent::Char('X' as u32),
            ..TraceCell::default()
        };
        g.set_cell(1, 0, cell).expect("valid set_cell");
        assert!(matches!(g.cells[1].content, TraceContent::Char(88)));
    }

    #[test]
    fn grid_set_cell_out_of_bounds() {
        let mut g = TraceGrid::new(2, 2);
        let err = g
            .set_cell(2, 0, TraceCell::default())
            .expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn grid_resize_replaces_contents() {
        let mut g = TraceGrid::new(2, 2);
        g.set_cell(
            0,
            0,
            TraceCell {
                content: TraceContent::Char('A' as u32),
                ..TraceCell::default()
            },
        )
        .unwrap();
        g.resize(3, 3);
        assert_eq!(g.width, 3);
        assert_eq!(g.height, 3);
        assert_eq!(g.cells.len(), 9);
        // All cells should be default after resize
        assert!(matches!(g.cells[0].content, TraceContent::Empty));
    }

    // ── Checksum ──────────────────────────────────────────────────────

    #[test]
    fn checksum_empty_grid_deterministic() {
        let g1 = TraceGrid::new(2, 2);
        let g2 = TraceGrid::new(2, 2);
        assert_eq!(g1.checksum(), g2.checksum());
    }

    #[test]
    fn checksum_differs_with_content() {
        let g1 = TraceGrid::new(1, 1);
        let mut g2 = TraceGrid::new(1, 1);
        g2.set_cell(
            0,
            0,
            TraceCell {
                content: TraceContent::Char('A' as u32),
                ..TraceCell::default()
            },
        )
        .unwrap();
        assert_ne!(g1.checksum(), g2.checksum());
    }

    #[test]
    fn checksum_differs_by_fg_color() {
        let mut g1 = TraceGrid::new(1, 1);
        let mut g2 = TraceGrid::new(1, 1);
        g1.set_cell(
            0,
            0,
            TraceCell {
                fg: 0xFF0000FF,
                ..TraceCell::default()
            },
        )
        .unwrap();
        g2.set_cell(
            0,
            0,
            TraceCell {
                fg: 0x00FF00FF,
                ..TraceCell::default()
            },
        )
        .unwrap();
        assert_ne!(g1.checksum(), g2.checksum());
    }

    #[test]
    fn checksum_grapheme_content() {
        let mut g = TraceGrid::new(1, 1);
        g.set_cell(
            0,
            0,
            TraceCell {
                content: TraceContent::Grapheme("⚙\u{fe0f}".as_bytes().to_vec()),
                ..TraceCell::default()
            },
        )
        .unwrap();
        let cs = g.checksum();
        // Verify determinism
        let mut g2 = TraceGrid::new(1, 1);
        g2.set_cell(
            0,
            0,
            TraceCell {
                content: TraceContent::Grapheme("⚙\u{fe0f}".as_bytes().to_vec()),
                ..TraceCell::default()
            },
        )
        .unwrap();
        assert_eq!(cs, g2.checksum());
    }

    #[test]
    fn checksum_continuation_differs_from_empty() {
        let g_empty = TraceGrid::new(1, 1);
        let mut g_cont = TraceGrid::new(1, 1);
        g_cont
            .set_cell(
                0,
                0,
                TraceCell {
                    content: TraceContent::Continuation,
                    ..TraceCell::default()
                },
            )
            .unwrap();
        assert_ne!(
            g_empty.checksum(),
            g_cont.checksum(),
            "continuation and empty should hash differently (different kind byte)"
        );
    }

    // ── read_* binary helpers ─────────────────────────────────────────

    #[test]
    fn read_u8_success() {
        let mut cursor = Cursor::new(vec![0x42]);
        assert_eq!(read_u8(&mut cursor).unwrap(), 0x42);
    }

    #[test]
    fn read_u8_empty_fails() {
        let mut cursor = Cursor::new(vec![]);
        assert!(read_u8(&mut cursor).is_err());
    }

    #[test]
    fn read_u16_le() {
        let mut cursor = Cursor::new(vec![0x34, 0x12]);
        assert_eq!(read_u16(&mut cursor).unwrap(), 0x1234);
    }

    #[test]
    fn read_u32_le() {
        let mut cursor = Cursor::new(vec![0x78, 0x56, 0x34, 0x12]);
        assert_eq!(read_u32(&mut cursor).unwrap(), 0x12345678);
    }

    #[test]
    fn read_u16_truncated_fails() {
        let mut cursor = Cursor::new(vec![0x34]);
        assert!(read_u16(&mut cursor).is_err());
    }

    // ── read_cell ─────────────────────────────────────────────────────

    #[test]
    fn read_cell_empty() {
        // kind=0, fg(4), bg(4), attrs(4) = 13 bytes
        let mut data = vec![0u8]; // kind = Empty
        data.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // fg
        data.extend_from_slice(&0x00000000u32.to_le_bytes()); // bg
        data.extend_from_slice(&0u32.to_le_bytes()); // attrs
        let mut cursor = Cursor::new(data);
        let cell = read_cell(&mut cursor).unwrap();
        assert!(matches!(cell.content, TraceContent::Empty));
        assert_eq!(cell.fg, 0xFFFFFFFF);
    }

    #[test]
    fn read_cell_char() {
        let mut data = vec![1u8]; // kind = Char
        data.extend_from_slice(&('Z' as u32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // fg
        data.extend_from_slice(&0u32.to_le_bytes()); // bg
        data.extend_from_slice(&0u32.to_le_bytes()); // attrs
        let mut cursor = Cursor::new(data);
        let cell = read_cell(&mut cursor).unwrap();
        assert!(matches!(cell.content, TraceContent::Char(90)));
    }

    #[test]
    fn read_cell_grapheme() {
        let grapheme = "🦀".as_bytes();
        let mut data = vec![2u8]; // kind = Grapheme
        data.extend_from_slice(&(grapheme.len() as u16).to_le_bytes());
        data.extend_from_slice(grapheme);
        data.extend_from_slice(&0u32.to_le_bytes()); // fg
        data.extend_from_slice(&0u32.to_le_bytes()); // bg
        data.extend_from_slice(&0u32.to_le_bytes()); // attrs
        let mut cursor = Cursor::new(data);
        let cell = read_cell(&mut cursor).unwrap();
        match &cell.content {
            TraceContent::Grapheme(bytes) => assert_eq!(bytes, grapheme),
            other => panic!("expected Grapheme, got {other:?}"),
        }
    }

    #[test]
    fn read_cell_continuation() {
        let mut data = vec![3u8]; // kind = Continuation
        data.extend_from_slice(&0u32.to_le_bytes()); // fg
        data.extend_from_slice(&0u32.to_le_bytes()); // bg
        data.extend_from_slice(&0u32.to_le_bytes()); // attrs
        let mut cursor = Cursor::new(data);
        let cell = read_cell(&mut cursor).unwrap();
        assert!(matches!(cell.content, TraceContent::Continuation));
    }

    #[test]
    fn read_cell_invalid_kind() {
        let mut data = vec![5u8]; // invalid kind
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        let mut cursor = Cursor::new(data);
        let err = read_cell(&mut cursor).expect_err("invalid kind");
        assert!(err.to_string().contains("invalid content_kind"));
    }

    #[test]
    fn read_cell_invalid_codepoint() {
        let mut data = vec![1u8]; // kind = Char
        data.extend_from_slice(&0xD800u32.to_le_bytes()); // surrogate, invalid
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        let mut cursor = Cursor::new(data);
        let err = read_cell(&mut cursor).expect_err("invalid codepoint");
        assert!(err.to_string().contains("invalid char codepoint"));
    }

    #[test]
    fn read_cell_grapheme_too_long() {
        let mut data = vec![2u8]; // kind = Grapheme
        data.extend_from_slice(&4097u16.to_le_bytes()); // exceeds 4096
        let mut cursor = Cursor::new(data);
        let err = read_cell(&mut cursor).expect_err("grapheme too long");
        assert!(err.to_string().contains("grapheme length exceeds 4096"));
    }

    // ── parse_* JSON helpers ──────────────────────────────────────────

    #[test]
    fn parse_u64_present() {
        let v: Value = serde_json::from_str(r#"{"x": 42}"#).unwrap();
        assert_eq!(parse_u64(&v, "x").unwrap(), 42);
    }

    #[test]
    fn parse_u64_missing() {
        let v: Value = serde_json::from_str(r#"{"y": 1}"#).unwrap();
        let err = parse_u64(&v, "x").expect_err("missing field");
        assert!(err.to_string().contains("missing x"));
    }

    #[test]
    fn parse_u64_string_type_fails() {
        let v: Value = serde_json::from_str(r#"{"x": "42"}"#).unwrap();
        let err = parse_u64(&v, "x").expect_err("wrong type");
        assert!(err.to_string().contains("missing x"));
    }

    #[test]
    fn parse_u16_in_range() {
        let v: Value = serde_json::from_str(r#"{"cols": 120}"#).unwrap();
        assert_eq!(parse_u16(&v, "cols").unwrap(), 120);
    }

    #[test]
    fn parse_u16_out_of_range() {
        let v: Value = serde_json::from_str(r#"{"cols": 70000}"#).unwrap();
        let err = parse_u16(&v, "cols").expect_err("out of range");
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn parse_str_present() {
        let v: Value = serde_json::from_str(r#"{"kind": "diff_runs_v1"}"#).unwrap();
        assert_eq!(parse_str(&v, "kind").unwrap(), "diff_runs_v1");
    }

    #[test]
    fn parse_str_missing() {
        let v: Value = serde_json::from_str(r#"{"other": 1}"#).unwrap();
        assert!(parse_str(&v, "kind").is_err());
    }

    #[test]
    fn parse_optional_str_present() {
        let v: Value = serde_json::from_str(r#"{"path": "frames/f0.bin"}"#).unwrap();
        assert_eq!(
            parse_optional_str(&v, "path"),
            Some("frames/f0.bin".to_string())
        );
    }

    #[test]
    fn parse_optional_str_missing() {
        let v: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(parse_optional_str(&v, "path"), None);
    }

    #[test]
    fn parse_hex_u64_valid() {
        assert_eq!(
            parse_hex_u64("0xcbf29ce484222325").unwrap(),
            0xcbf29ce484222325
        );
    }

    #[test]
    fn parse_hex_u64_no_prefix() {
        assert_eq!(
            parse_hex_u64("cbf29ce484222325").unwrap(),
            0xcbf29ce484222325
        );
    }

    #[test]
    fn parse_hex_u64_wrong_length() {
        let err = parse_hex_u64("0xabc").expect_err("wrong length");
        assert!(err.to_string().contains("16 hex chars"));
    }

    #[test]
    fn parse_hex_u64_invalid_chars() {
        let err = parse_hex_u64("zzzzzzzzzzzzzzzz").expect_err("invalid hex");
        assert!(err.to_string().contains("invalid checksum"));
    }

    // ── resolve_payload_path ──────────────────────────────────────────

    #[test]
    fn resolve_payload_path_relative() {
        let base = Path::new("/trace/output");
        let result = resolve_payload_path(base, "frames/f0.bin");
        assert_eq!(result, PathBuf::from("/trace/output/frames/f0.bin"));
    }

    #[test]
    fn resolve_payload_path_absolute() {
        let base = Path::new("/trace/output");
        let result = resolve_payload_path(base, "/other/path/f0.bin");
        assert_eq!(result, PathBuf::from("/other/path/f0.bin"));
    }

    // ── apply_diff_runs ───────────────────────────────────────────────

    fn build_diff_runs_payload(
        width: u16,
        height: u16,
        runs: &[(u16, u16, u16, Vec<u8>)],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.extend_from_slice(&(runs.len() as u32).to_le_bytes());
        for (y, x0, x1, cell_data) in runs {
            buf.extend_from_slice(&y.to_le_bytes());
            buf.extend_from_slice(&x0.to_le_bytes());
            buf.extend_from_slice(&x1.to_le_bytes());
            buf.extend_from_slice(cell_data);
        }
        buf
    }

    fn empty_cell_bytes() -> Vec<u8> {
        let mut data = vec![0u8]; // kind=Empty
        data.extend_from_slice(&ftui_render::cell::PackedRgba::WHITE.0.to_le_bytes());
        data.extend_from_slice(&ftui_render::cell::PackedRgba::TRANSPARENT.0.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data
    }

    fn char_cell_bytes(ch: char) -> Vec<u8> {
        let mut data = vec![1u8]; // kind=Char
        data.extend_from_slice(&(ch as u32).to_le_bytes());
        data.extend_from_slice(&ftui_render::cell::PackedRgba::WHITE.0.to_le_bytes());
        data.extend_from_slice(&ftui_render::cell::PackedRgba::TRANSPARENT.0.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data
    }

    #[test]
    fn apply_diff_runs_single_cell() {
        let mut grid = TraceGrid::new(2, 2);
        let cell_data = char_cell_bytes('A');
        let payload = build_diff_runs_payload(2, 2, &[(0, 0, 0, cell_data)]);
        let stats = grid.apply_diff_runs(&payload).unwrap();
        assert_eq!(stats.runs, 1);
        assert_eq!(stats.cells, 1);
        assert!(matches!(grid.cells[0].content, TraceContent::Char(65)));
    }

    #[test]
    fn apply_diff_runs_dimension_mismatch() {
        let mut grid = TraceGrid::new(2, 2);
        let payload = build_diff_runs_payload(3, 2, &[]);
        let err = grid.apply_diff_runs(&payload).expect_err("mismatch");
        assert!(err.to_string().contains("dimensions do not match"));
    }

    #[test]
    fn apply_diff_runs_invalid_range() {
        let mut grid = TraceGrid::new(4, 4);
        // x1 < x0 is invalid
        let cell_data = char_cell_bytes('A');
        let payload = build_diff_runs_payload(4, 4, &[(0, 3, 1, cell_data)]);
        let err = grid.apply_diff_runs(&payload).expect_err("invalid range");
        assert!(err.to_string().contains("invalid run range"));
    }

    #[test]
    fn apply_diff_runs_out_of_bounds() {
        let mut grid = TraceGrid::new(2, 2);
        let cell_data = char_cell_bytes('A');
        // y=2 is out of bounds for height=2
        let payload = build_diff_runs_payload(2, 2, &[(2, 0, 0, cell_data)]);
        let err = grid.apply_diff_runs(&payload).expect_err("out of bounds");
        assert!(err.to_string().contains("run out of bounds"));
    }

    #[test]
    fn apply_diff_runs_trailing_bytes() {
        let mut grid = TraceGrid::new(2, 2);
        let cell_data = char_cell_bytes('A');
        let mut payload = build_diff_runs_payload(2, 2, &[(0, 0, 0, cell_data)]);
        payload.push(0xFF); // trailing byte
        let err = grid.apply_diff_runs(&payload).expect_err("trailing bytes");
        assert!(err.to_string().contains("trailing bytes"));
    }

    // ── apply_full_buffer ─────────────────────────────────────────────

    fn build_full_buffer_payload(width: u16, height: u16, cells: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        for cell_data in cells {
            buf.extend_from_slice(cell_data);
        }
        buf
    }

    #[test]
    fn apply_full_buffer_1x1() {
        let mut grid = TraceGrid::new(1, 1);
        let payload = build_full_buffer_payload(1, 1, &[char_cell_bytes('X')]);
        let stats = grid.apply_full_buffer(&payload).unwrap();
        assert_eq!(stats.cells, 1);
        assert_eq!(stats.runs, 1); // runs == height for full buffer
        assert!(matches!(grid.cells[0].content, TraceContent::Char(88)));
    }

    #[test]
    fn apply_full_buffer_2x2() {
        let mut grid = TraceGrid::new(2, 2);
        let cells = vec![
            char_cell_bytes('A'),
            char_cell_bytes('B'),
            char_cell_bytes('C'),
            char_cell_bytes('D'),
        ];
        let payload = build_full_buffer_payload(2, 2, &cells);
        let stats = grid.apply_full_buffer(&payload).unwrap();
        assert_eq!(stats.cells, 4);
        assert!(matches!(grid.cells[0].content, TraceContent::Char(65)));
        assert!(matches!(grid.cells[3].content, TraceContent::Char(68)));
    }

    #[test]
    fn apply_full_buffer_dimension_mismatch() {
        let mut grid = TraceGrid::new(2, 2);
        let cells: Vec<Vec<u8>> = (0..6).map(|_| empty_cell_bytes()).collect();
        let payload = build_full_buffer_payload(3, 2, &cells);
        let err = grid.apply_full_buffer(&payload).expect_err("mismatch");
        assert!(err.to_string().contains("dimensions do not match"));
    }

    #[test]
    fn apply_full_buffer_trailing_bytes() {
        let mut grid = TraceGrid::new(1, 1);
        let mut payload = build_full_buffer_payload(1, 1, &[char_cell_bytes('A')]);
        payload.push(0xFF);
        let err = grid
            .apply_full_buffer(&payload)
            .expect_err("trailing bytes");
        assert!(err.to_string().contains("trailing bytes"));
    }

    // ── Checksum consistency between diff_runs and full_buffer ────────

    #[test]
    fn checksum_matches_between_apply_methods() {
        // Build a 2x2 grid with 'A' at (0,0) via diff_runs
        let mut g1 = TraceGrid::new(2, 2);
        let cell_data = char_cell_bytes('A');
        let diff_payload = build_diff_runs_payload(2, 2, &[(0, 0, 0, cell_data)]);
        g1.apply_diff_runs(&diff_payload).unwrap();

        // Build same grid via full_buffer (A at 0, empty at 1,2,3)
        let mut g2 = TraceGrid::new(2, 2);
        let cells = vec![
            char_cell_bytes('A'),
            empty_cell_bytes(),
            empty_cell_bytes(),
            empty_cell_bytes(),
        ];
        let full_payload = build_full_buffer_payload(2, 2, &cells);
        g2.apply_full_buffer(&full_payload).unwrap();

        assert_eq!(
            g1.checksum(),
            g2.checksum(),
            "same grid content should produce same checksum regardless of apply method"
        );
    }

    // ── Failing-trace minimizer ──────────────────────────────────────

    fn unique_test_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir.push(format!(
            "ftui-trace-replay-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn frame_line(frame_idx: u64, cols: u16, rows: u16, checksum: u64) -> String {
        format!(
            "{{\"event\":\"frame\",\"frame_idx\":{frame_idx},\"cols\":{cols},\"rows\":{rows},\"payload_kind\":\"none\",\"checksum\":\"{checksum:016x}\"}}"
        )
    }

    fn write_lines(path: &Path, lines: &[String]) {
        let mut file = File::create(path).expect("create trace file");
        for line in lines {
            writeln!(file, "{line}").expect("write trace line");
        }
    }

    #[test]
    fn minimize_failing_trace_reduces_and_preserves_checksum_mismatch() {
        let dir = unique_test_dir("minimize");
        let input = dir.join("input.jsonl");
        let output = dir.join("output.min.jsonl");
        let checksum = TraceGrid::new(1, 1).checksum();
        let mut lines = vec!["{\"event\":\"meta\",\"version\":1}".to_string()];
        for frame_idx in 0..20 {
            lines.push(frame_line(frame_idx, 1, 1, checksum));
        }
        lines.push(frame_line(20, 1, 1, 0xDEADBEEFDEADBEEF)); // failing frame
        lines.push(frame_line(21, 1, 1, checksum));
        write_lines(&input, &lines);

        let options = MinimizeOptions {
            max_attempts: 256,
            max_duration: Some(Duration::from_secs(2)),
            ..MinimizeOptions::default()
        };
        let report = minimize_failing_trace(&input, &output, options).expect("minimize succeeds");

        assert_eq!(report.baseline_class, "checksum_mismatch");
        assert_eq!(
            classify_replay_error(&report.final_error),
            "checksum_mismatch"
        );
        assert!(
            report.minimized_lines < report.original_lines,
            "expected minimization to reduce line count"
        );
        assert!(
            report.original_lines as f64 / report.minimized_lines as f64 >= 10.0,
            "expected >=10x reduction"
        );
        assert!(report.attempts > 0);
        assert!(report.preserved_attempts > 0);
        assert!(
            replay_trace(&output).is_err(),
            "minimized trace must still fail"
        );
    }

    #[test]
    fn minimizer_is_deterministic_for_same_input() {
        let dir = unique_test_dir("deterministic");
        let input = dir.join("input.jsonl");
        let out_a = dir.join("a.min.jsonl");
        let out_b = dir.join("b.min.jsonl");
        let checksum = TraceGrid::new(1, 1).checksum();
        let lines = vec![
            frame_line(0, 1, 1, checksum),
            frame_line(1, 1, 1, checksum),
            frame_line(2, 1, 1, checksum),
            frame_line(3, 1, 1, 0xBAD0BAD0BAD0BAD0),
        ];
        write_lines(&input, &lines);

        let options = MinimizeOptions {
            max_attempts: 256,
            max_duration: Some(Duration::from_secs(2)),
            ..MinimizeOptions::default()
        };
        let report_a =
            minimize_failing_trace(&input, &out_a, options).expect("first minimize succeeds");
        let report_b =
            minimize_failing_trace(&input, &out_b, options).expect("second minimize succeeds");
        let bytes_a = std::fs::read(&out_a).expect("read output A");
        let bytes_b = std::fs::read(&out_b).expect("read output B");

        assert_eq!(bytes_a, bytes_b, "minimized traces must be byte-identical");
        assert_eq!(report_a.minimized_lines, report_b.minimized_lines);
        assert_eq!(report_a.baseline_class, report_b.baseline_class);
    }

    #[test]
    fn minimization_report_writers_emit_valid_json() {
        let dir = unique_test_dir("report");
        let report_path = dir.join("report.json");
        let ledger_path = dir.join("report.jsonl");
        let report = MinimizeReport {
            input_path: PathBuf::from("in.jsonl"),
            output_path: PathBuf::from("out.jsonl"),
            baseline_error: "checksum mismatch".to_string(),
            baseline_class: "checksum_mismatch".to_string(),
            final_error: "checksum mismatch".to_string(),
            original_lines: 100,
            minimized_lines: 10,
            attempts: 12,
            preserved_attempts: 4,
            duration_ms: 123,
            ledger: vec![MinimizeAttempt {
                attempt: 1,
                granularity: 2,
                removed_lines: 50,
                candidate_lines: 50,
                replay_ms: 7,
                replay_class: "checksum_mismatch".to_string(),
                preserved_failure: true,
                posterior_mean: 0.25,
                posterior_variance: 0.01,
                voi_score: 0.5,
            }],
        };

        write_minimization_report_json(&report_path, &report).expect("write json report");
        write_minimization_report_jsonl(&ledger_path, &report).expect("write jsonl report");

        let json: Value = serde_json::from_slice(&std::fs::read(&report_path).expect("read json"))
            .expect("parse report json");
        assert_eq!(json["baseline_class"], "checksum_mismatch");
        let jsonl_text = std::fs::read_to_string(&ledger_path).expect("read jsonl");
        assert!(jsonl_text.contains("\"event\":\"minimize_attempt\""));
    }
}
