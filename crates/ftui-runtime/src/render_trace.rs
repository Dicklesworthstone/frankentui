#![forbid(unsafe_code)]

//! Render-trace recorder for deterministic replay (bd-3e1t.4.13).
//!
//! Emits JSONL records following the render-trace v1 schema in
//! `docs/spec/state-machines.md`:
//! - header (event="trace_header")
//! - frame (event="frame")
//! - summary (event="trace_summary")

use std::fs::{OpenOptions, create_dir_all};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, CellContent};
use ftui_render::diff::BufferDiff;
use ftui_render::grapheme_pool::GraphemePool;

use crate::conformal_predictor::ConformalConfig;
use crate::resize_coalescer::CoalescerConfig;
use crate::terminal_writer::RuntimeDiffConfig;

/// Configuration for render-trace recording.
#[derive(Debug, Clone)]
pub struct RenderTraceConfig {
    /// Enable render-trace recording.
    pub enabled: bool,
    /// Output JSONL path (trace.jsonl).
    pub output_path: PathBuf,
    /// Optional run identifier override.
    pub run_id: Option<String>,
    /// Optional deterministic seed (or null).
    pub seed: Option<u64>,
    /// Optional test module label (or null).
    pub test_module: Option<String>,
    /// Flush after every JSONL line.
    pub flush_on_write: bool,
    /// Include start_ts_ms in header (non-deterministic if true).
    pub include_start_ts_ms: bool,
}

impl Default for RenderTraceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output_path: PathBuf::from("trace.jsonl"),
            run_id: None,
            seed: None,
            test_module: None,
            flush_on_write: true,
            include_start_ts_ms: false,
        }
    }
}

impl RenderTraceConfig {
    /// Enable render-trace recording to the given path.
    #[must_use]
    pub fn enabled_file(path: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            output_path: path.into(),
            ..Default::default()
        }
    }

    /// Set a run identifier.
    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Set a deterministic seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set a test module label.
    #[must_use]
    pub fn with_test_module(mut self, test_module: impl Into<String>) -> Self {
        self.test_module = Some(test_module.into());
        self
    }

    /// Toggle flush-on-write.
    #[must_use]
    pub fn with_flush_on_write(mut self, enabled: bool) -> Self {
        self.flush_on_write = enabled;
        self
    }

    /// Include `start_ts_ms` in header (non-deterministic).
    #[must_use]
    pub fn with_start_ts_ms(mut self, enabled: bool) -> Self {
        self.include_start_ts_ms = enabled;
        self
    }
}

/// Context used to build a render-trace header.
#[derive(Debug, Clone)]
pub struct RenderTraceContext<'a> {
    pub capabilities: &'a TerminalCapabilities,
    pub diff_config: RuntimeDiffConfig,
    pub resize_config: CoalescerConfig,
    pub conformal_config: Option<ConformalConfig>,
}

/// Render-trace recorder.
pub struct RenderTraceRecorder {
    writer: BufWriter<std::fs::File>,
    flush_on_write: bool,
    frame_idx: u64,
    checksum_chain: u64,
    total_frames: u64,
    finished: bool,
    payload_dir: Option<PayloadDir>,
}

#[derive(Debug, Clone)]
struct PayloadDir {
    abs: PathBuf,
    rel: String,
}

/// Payload kind for render-trace frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTracePayloadKind {
    DiffRunsV1,
    FullBufferV1,
}

impl RenderTracePayloadKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiffRunsV1 => "diff_runs_v1",
            Self::FullBufferV1 => "full_buffer_v1",
        }
    }
}

/// Render-trace payload bytes with its kind.
#[derive(Debug, Clone)]
pub struct RenderTracePayload {
    pub kind: RenderTracePayloadKind,
    pub bytes: Vec<u8>,
}

/// Payload metadata written to disk.
#[derive(Debug, Clone)]
pub struct RenderTracePayloadInfo {
    pub kind: &'static str,
    pub path: String,
}

impl RenderTraceRecorder {
    /// Build a recorder from config. Returns `Ok(None)` when disabled.
    pub fn from_config(
        config: &RenderTraceConfig,
        context: RenderTraceContext<'_>,
    ) -> io::Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }

        let base_dir = config
            .output_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = config
            .output_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("trace");
        let payload_dir_name = format!("{stem}_payloads");
        let payload_dir_abs = base_dir.join(&payload_dir_name);
        create_dir_all(&payload_dir_abs)?;

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&config.output_path)?;
        let mut recorder = Self {
            writer: BufWriter::new(file),
            flush_on_write: config.flush_on_write,
            frame_idx: 0,
            checksum_chain: 0,
            total_frames: 0,
            finished: false,
            payload_dir: Some(PayloadDir {
                abs: payload_dir_abs,
                rel: payload_dir_name,
            }),
        };

        let run_id = config
            .run_id
            .clone()
            .unwrap_or_else(default_render_trace_run_id);
        let env = RenderTraceEnv::new(config.test_module.clone());
        let caps = RenderTraceCapabilities::from_caps(context.capabilities);
        let policies = RenderTracePolicies::from_context(&context);
        let start_ts_ms = if config.include_start_ts_ms {
            Some(now_ms())
        } else {
            None
        };
        let header = RenderTraceHeader {
            run_id,
            seed: config.seed,
            env,
            capabilities: caps,
            policies,
            start_ts_ms,
        };
        recorder.write_jsonl(&header.to_jsonl())?;
        Ok(Some(recorder))
    }

    /// Write a payload blob to the payload directory and return metadata.
    pub fn write_payload(
        &mut self,
        payload: &RenderTracePayload,
    ) -> io::Result<RenderTracePayloadInfo> {
        let Some(dir) = &self.payload_dir else {
            return Err(io::Error::other(
                "render-trace payload directory unavailable",
            ));
        };
        let file_name = format!("frame_{:06}_{}.bin", self.frame_idx, payload.kind.as_str());
        let abs_path = dir.abs.join(&file_name);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&abs_path)?;
        file.write_all(&payload.bytes)?;
        if self.flush_on_write {
            file.flush()?;
        }
        Ok(RenderTracePayloadInfo {
            kind: payload.kind.as_str(),
            path: format!("{}/{}", dir.rel, file_name),
        })
    }

    /// Record a frame.
    pub fn record_frame(
        &mut self,
        mut frame: RenderTraceFrame<'_>,
        buffer: &Buffer,
        pool: &GraphemePool,
    ) -> io::Result<()> {
        let trace_start = Instant::now();
        let checksum = checksum_buffer(buffer, pool);
        let checksum_chain = fnv1a64_pair(self.checksum_chain, checksum);
        frame.trace_us = Some(trace_start.elapsed().as_micros() as u64);

        let line = frame.to_jsonl(self.frame_idx, checksum, checksum_chain);
        self.write_jsonl(&line)?;

        self.frame_idx = self.frame_idx.saturating_add(1);
        self.checksum_chain = checksum_chain;
        self.total_frames = self.total_frames.saturating_add(1);
        Ok(())
    }

    /// Finish recording and write summary.
    pub fn finish(&mut self, elapsed_ms: Option<u64>) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        let summary = RenderTraceSummary {
            total_frames: self.total_frames,
            final_checksum_chain: self.checksum_chain,
            elapsed_ms,
        };
        self.write_jsonl(&summary.to_jsonl())?;
        self.finished = true;
        Ok(())
    }

    fn write_jsonl(&mut self, line: &str) -> io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        if self.flush_on_write {
            self.writer.flush()?;
        }
        Ok(())
    }
}

/// Render-trace header record.
#[derive(Debug, Clone)]
struct RenderTraceHeader {
    run_id: String,
    seed: Option<u64>,
    env: RenderTraceEnv,
    capabilities: RenderTraceCapabilities,
    policies: RenderTracePolicies,
    start_ts_ms: Option<u64>,
}

impl RenderTraceHeader {
    fn to_jsonl(&self) -> String {
        let seed = opt_u64(self.seed);
        let start_ts = opt_u64(self.start_ts_ms);
        format!(
            concat!(
                r#"{{"event":"trace_header","schema_version":"render-trace-v1","#,
                r#""run_id":"{}","seed":{},"env":{},"capabilities":{},"policies":{},"start_ts_ms":{}}}"#
            ),
            json_escape(&self.run_id),
            seed,
            self.env.to_json(),
            self.capabilities.to_json(),
            self.policies.to_json(),
            start_ts
        )
    }
}

/// Render-trace frame record.
#[derive(Debug, Clone)]
pub struct RenderTraceFrame<'a> {
    pub cols: u16,
    pub rows: u16,
    pub mode: &'a str,
    pub ui_height: u16,
    pub ui_anchor: &'a str,
    pub diff_strategy: &'a str,
    pub diff_cells: usize,
    pub diff_runs: usize,
    pub present_bytes: u64,
    pub render_us: Option<u64>,
    pub present_us: Option<u64>,
    pub payload_kind: &'a str,
    pub payload_path: Option<&'a str>,
    pub trace_us: Option<u64>,
}

impl RenderTraceFrame<'_> {
    fn to_jsonl(&self, frame_idx: u64, checksum: u64, checksum_chain: u64) -> String {
        let render_us = opt_u64(self.render_us);
        let present_us = opt_u64(self.present_us);
        let payload_path = opt_str(self.payload_path);
        let trace_us = opt_u64(self.trace_us);
        format!(
            concat!(
                r#"{{"event":"frame","frame_idx":{},"cols":{},"rows":{},"mode":"{}","#,
                r#""ui_height":{},"ui_anchor":"{}","diff_strategy":"{}","diff_cells":{},"diff_runs":{},"present_bytes":{},"render_us":{},"present_us":{},"checksum":"{:016x}","checksum_chain":"{:016x}","payload_kind":"{}","payload_path":{},"trace_us":{}}}"#
            ),
            frame_idx,
            self.cols,
            self.rows,
            json_escape(self.mode),
            self.ui_height,
            json_escape(self.ui_anchor),
            json_escape(self.diff_strategy),
            self.diff_cells,
            self.diff_runs,
            self.present_bytes,
            render_us,
            present_us,
            checksum,
            checksum_chain,
            json_escape(self.payload_kind),
            payload_path,
            trace_us
        )
    }
}

/// Render-trace summary record.
#[derive(Debug, Clone)]
struct RenderTraceSummary {
    total_frames: u64,
    final_checksum_chain: u64,
    elapsed_ms: Option<u64>,
}

impl RenderTraceSummary {
    fn to_jsonl(&self) -> String {
        let elapsed_ms = opt_u64(self.elapsed_ms);
        format!(
            r#"{{"event":"trace_summary","total_frames":{},"final_checksum_chain":"{:016x}","elapsed_ms":{}}}"#,
            self.total_frames, self.final_checksum_chain, elapsed_ms
        )
    }
}

#[derive(Debug, Clone)]
struct RenderTraceEnv {
    os: String,
    arch: String,
    test_module: Option<String>,
}

impl RenderTraceEnv {
    fn new(test_module: Option<String>) -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            test_module,
        }
    }

    fn to_json(&self) -> String {
        format!(
            r#"{{"os":"{}","arch":"{}","test_module":{}}}"#,
            json_escape(&self.os),
            json_escape(&self.arch),
            opt_str(self.test_module.as_deref())
        )
    }
}

#[derive(Debug, Clone)]
struct RenderTraceCapabilities {
    profile: String,
    true_color: bool,
    colors_256: bool,
    sync_output: bool,
    osc8_hyperlinks: bool,
    scroll_region: bool,
    in_tmux: bool,
    in_screen: bool,
    in_zellij: bool,
    kitty_keyboard: bool,
    focus_events: bool,
    bracketed_paste: bool,
    mouse_sgr: bool,
    osc52_clipboard: bool,
}

impl RenderTraceCapabilities {
    fn from_caps(caps: &TerminalCapabilities) -> Self {
        Self {
            profile: caps.profile().as_str().to_string(),
            true_color: caps.true_color,
            colors_256: caps.colors_256,
            sync_output: caps.sync_output,
            osc8_hyperlinks: caps.osc8_hyperlinks,
            scroll_region: caps.scroll_region,
            in_tmux: caps.in_tmux,
            in_screen: caps.in_screen,
            in_zellij: caps.in_zellij,
            kitty_keyboard: caps.kitty_keyboard,
            focus_events: caps.focus_events,
            bracketed_paste: caps.bracketed_paste,
            mouse_sgr: caps.mouse_sgr,
            osc52_clipboard: caps.osc52_clipboard,
        }
    }

    fn to_json(&self) -> String {
        format!(
            concat!(
                r#"{{"profile":"{}","true_color":{},"colors_256":{},"sync_output":{},"osc8_hyperlinks":{},"scroll_region":{},"in_tmux":{},"in_screen":{},"in_zellij":{},"kitty_keyboard":{},"focus_events":{},"bracketed_paste":{},"mouse_sgr":{},"osc52_clipboard":{}}}"#
            ),
            json_escape(&self.profile),
            self.true_color,
            self.colors_256,
            self.sync_output,
            self.osc8_hyperlinks,
            self.scroll_region,
            self.in_tmux,
            self.in_screen,
            self.in_zellij,
            self.kitty_keyboard,
            self.focus_events,
            self.bracketed_paste,
            self.mouse_sgr,
            self.osc52_clipboard
        )
    }
}

#[derive(Debug, Clone)]
struct RenderTracePolicies {
    diff_bayesian: bool,
    diff_dirty_rows: bool,
    diff_dirty_spans: bool,
    diff_guard_band: u16,
    diff_merge_gap: u16,
    bocpd_enabled: bool,
    steady_delay_ms: u64,
    burst_delay_ms: u64,
    conformal_enabled: bool,
    conformal_alpha: Option<f64>,
    conformal_min_samples: Option<usize>,
    conformal_window_size: Option<usize>,
}

impl RenderTracePolicies {
    fn from_context(context: &RenderTraceContext) -> Self {
        let diff = &context.diff_config;
        let span = diff.dirty_span_config;
        let resize = &context.resize_config;
        let conformal = context.conformal_config.as_ref();
        Self {
            diff_bayesian: diff.bayesian_enabled,
            diff_dirty_rows: diff.dirty_rows_enabled,
            diff_dirty_spans: span.enabled,
            diff_guard_band: span.guard_band,
            diff_merge_gap: span.merge_gap,
            bocpd_enabled: resize.enable_bocpd,
            steady_delay_ms: resize.steady_delay_ms,
            burst_delay_ms: resize.burst_delay_ms,
            conformal_enabled: conformal.is_some(),
            conformal_alpha: conformal.map(|c| c.alpha),
            conformal_min_samples: conformal.map(|c| c.min_samples),
            conformal_window_size: conformal.map(|c| c.window_size),
        }
    }

    fn to_json(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(256);
        out.push('{');
        out.push_str("\"diff\":{");
        let _ = write!(
            out,
            "\"bayesian\":{},\"dirty_rows\":{},\"dirty_spans\":{},\"guard_band\":{},\"merge_gap\":{}",
            self.diff_bayesian,
            self.diff_dirty_rows,
            self.diff_dirty_spans,
            self.diff_guard_band,
            self.diff_merge_gap
        );
        out.push('}');
        out.push(',');
        out.push_str("\"bocpd\":{");
        let _ = write!(
            out,
            "\"enabled\":{},\"steady_delay_ms\":{},\"burst_delay_ms\":{}",
            self.bocpd_enabled, self.steady_delay_ms, self.burst_delay_ms
        );
        out.push('}');
        out.push(',');
        out.push_str("\"conformal\":{");
        let _ = write!(
            out,
            "\"enabled\":{},\"alpha\":{},\"min_samples\":{},\"window_size\":{}",
            self.conformal_enabled,
            opt_f64(self.conformal_alpha),
            opt_usize(self.conformal_min_samples),
            opt_usize(self.conformal_window_size)
        );
        out.push('}');
        out.push('}');
        out
    }
}

/// Deterministic FNV-1a checksum of a buffer grid.
#[must_use]
pub fn checksum_buffer(buffer: &Buffer, pool: &GraphemePool) -> u64 {
    let width = buffer.width();
    let height = buffer.height();

    let mut hash = FNV_OFFSET_BASIS;
    for y in 0..height {
        for x in 0..width {
            let cell = buffer.get_unchecked(x, y);
            match cell.content {
                CellContent::EMPTY => {
                    hash = fnv1a64_byte(hash, 0u8);
                    hash = fnv1a64_u16(hash, 0);
                }
                CellContent::CONTINUATION => {
                    hash = fnv1a64_byte(hash, 3u8);
                    hash = fnv1a64_u16(hash, 0);
                }
                content => {
                    if let Some(ch) = content.as_char() {
                        hash = fnv1a64_byte(hash, 1u8);
                        let mut buf = [0u8; 4];
                        let encoded = ch.encode_utf8(&mut buf);
                        let bytes = encoded.as_bytes();
                        let len = bytes.len().min(u16::MAX as usize) as u16;
                        hash = fnv1a64_u16(hash, len);
                        hash = fnv1a64_bytes(hash, &bytes[..len as usize]);
                    } else if let Some(gid) = content.grapheme_id() {
                        hash = fnv1a64_byte(hash, 2u8);
                        let text = pool.get(gid).unwrap_or("");
                        let bytes = text.as_bytes();
                        let len = bytes.len().min(u16::MAX as usize) as u16;
                        hash = fnv1a64_u16(hash, len);
                        hash = fnv1a64_bytes(hash, &bytes[..len as usize]);
                    } else {
                        hash = fnv1a64_byte(hash, 0u8);
                        hash = fnv1a64_u16(hash, 0);
                    }
                }
            }

            hash = fnv1a64_u32(hash, cell.fg.0);
            hash = fnv1a64_u32(hash, cell.bg.0);
            let attrs = pack_attrs(cell.attrs);
            hash = fnv1a64_u32(hash, attrs);
        }
    }
    hash
}

/// Encode a buffer into a full-buffer payload.
#[must_use]
pub fn build_full_buffer_payload(buffer: &Buffer, pool: &GraphemePool) -> RenderTracePayload {
    let width = buffer.width();
    let height = buffer.height();
    let mut bytes = Vec::with_capacity(4 + (width as usize * height as usize * 16));
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    for y in 0..height {
        for x in 0..width {
            let cell = buffer.get_unchecked(x, y);
            push_cell_bytes(&mut bytes, cell, pool);
        }
    }
    RenderTracePayload {
        kind: RenderTracePayloadKind::FullBufferV1,
        bytes,
    }
}

/// Encode diff runs into a payload.
#[must_use]
pub fn build_diff_runs_payload(
    buffer: &Buffer,
    diff: &BufferDiff,
    pool: &GraphemePool,
) -> RenderTracePayload {
    let width = buffer.width();
    let height = buffer.height();
    let runs = diff.runs();
    let mut bytes = Vec::with_capacity(12 + runs.len() * 24);
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    let run_count = runs.len() as u32;
    bytes.extend_from_slice(&run_count.to_le_bytes());
    for run in runs {
        bytes.extend_from_slice(&run.y.to_le_bytes());
        bytes.extend_from_slice(&run.x0.to_le_bytes());
        bytes.extend_from_slice(&run.x1.to_le_bytes());
        for x in run.x0..=run.x1 {
            let cell = buffer.get_unchecked(x, run.y);
            push_cell_bytes(&mut bytes, cell, pool);
        }
    }
    RenderTracePayload {
        kind: RenderTracePayloadKind::DiffRunsV1,
        bytes,
    }
}

fn pack_attrs(attrs: CellAttrs) -> u32 {
    let flags = attrs.flags().bits() as u32;
    let link = attrs.link_id() & 0x00FF_FFFF;
    (flags << 24) | link
}

fn push_cell_bytes(out: &mut Vec<u8>, cell: &Cell, pool: &GraphemePool) {
    match cell.content {
        CellContent::EMPTY => {
            out.push(0u8);
        }
        CellContent::CONTINUATION => {
            out.push(3u8);
        }
        content => {
            if let Some(ch) = content.as_char() {
                out.push(1u8);
                out.extend_from_slice(&(ch as u32).to_le_bytes());
            } else if let Some(gid) = content.grapheme_id() {
                out.push(2u8);
                let text = pool.get(gid).unwrap_or("");
                let bytes = text.as_bytes();
                let len = bytes.len().min(u16::MAX as usize) as u16;
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(&bytes[..len as usize]);
            } else {
                out.push(0u8);
            }
        }
    }
    out.extend_from_slice(&cell.fg.0.to_le_bytes());
    out.extend_from_slice(&cell.bg.0.to_le_bytes());
    let attrs = pack_attrs(cell.attrs);
    out.extend_from_slice(&attrs.to_le_bytes());
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a64_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    let len = bytes.len();
    while i + 8 <= len {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 1] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 2] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 3] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 4] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 5] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 6] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        hash ^= bytes[i + 7] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 8;
    }
    for &b in &bytes[i..] {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fnv1a64_byte(hash: u64, b: u8) -> u64 {
    let mut hash = hash ^ (b as u64);
    hash = hash.wrapping_mul(FNV_PRIME);
    hash
}

fn fnv1a64_u16(hash: u64, v: u16) -> u64 {
    fnv1a64_bytes(hash, &v.to_le_bytes())
}

fn fnv1a64_u32(hash: u64, v: u32) -> u64 {
    fnv1a64_bytes(hash, &v.to_le_bytes())
}

fn fnv1a64_pair(prev: u64, next: u64) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash = fnv1a64_u64(hash, prev);
    fnv1a64_u64(hash, next)
}

fn fnv1a64_u64(hash: u64, v: u64) -> u64 {
    fnv1a64_bytes(hash, &v.to_le_bytes())
}

fn default_render_trace_run_id() -> String {
    format!("render-trace-{}", std::process::id())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn opt_u64(v: Option<u64>) -> String {
    v.map_or_else(|| "null".to_string(), |v| v.to_string())
}

fn opt_usize(v: Option<usize>) -> String {
    v.map_or_else(|| "null".to_string(), |v| v.to_string())
}

fn opt_f64(v: Option<f64>) -> String {
    v.map_or_else(|| "null".to_string(), |v| format!("{v:.6}"))
}

fn opt_str(v: Option<&str>) -> String {
    v.map_or_else(|| "null".to_string(), |s| format!("\"{}\"", json_escape(s)))
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::buffer::Buffer;
    use ftui_render::cell::Cell;

    fn temp_trace_path(label: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ftui_render_trace_{}_{}_{}.jsonl",
            label,
            std::process::id(),
            id
        ));
        path
    }

    #[test]
    fn checksum_is_deterministic() {
        let mut buffer = Buffer::new(4, 2);
        buffer.set(0, 0, Cell::from_char('A'));
        buffer.set(1, 0, Cell::from_char('B'));
        let pool = GraphemePool::new();
        let a = checksum_buffer(&buffer, &pool);
        let b = checksum_buffer(&buffer, &pool);
        assert_eq!(a, b);
    }

    #[test]
    fn recorder_writes_header_frame_summary() {
        let path = temp_trace_path("basic");
        let config = RenderTraceConfig::enabled_file(&path);
        let caps = TerminalCapabilities::default();
        let context = RenderTraceContext {
            capabilities: &caps,
            diff_config: RuntimeDiffConfig::default(),
            resize_config: CoalescerConfig::default(),
            conformal_config: None,
        };
        let mut recorder = RenderTraceRecorder::from_config(&config, context)
            .expect("config")
            .expect("enabled");

        let buffer = Buffer::new(2, 2);
        let pool = GraphemePool::new();
        let frame = RenderTraceFrame {
            cols: 2,
            rows: 2,
            mode: "inline",
            ui_height: 2,
            ui_anchor: "bottom",
            diff_strategy: "full",
            diff_cells: 4,
            diff_runs: 2,
            present_bytes: 16,
            render_us: None,
            present_us: Some(10),
            payload_kind: "none",
            payload_path: None,
            trace_us: Some(2),
        };

        recorder.record_frame(frame, &buffer, &pool).expect("frame");
        recorder.finish(Some(42)).expect("finish");

        let text = std::fs::read_to_string(path).expect("read");
        assert!(text.contains("\"event\":\"trace_header\""));
        assert!(text.contains("\"event\":\"frame\""));
        assert!(text.contains("\"event\":\"trace_summary\""));
    }
}
