#![forbid(unsafe_code)]

//! Platform-independent runner core wrapping `StepProgram<AppModel>`.
//!
//! This module contains the logic shared between the wasm-bindgen exports
//! and the native test harness. No JS/WASM types here.

use core::time::Duration;

use ftui_demo_showcase::app::AppModel;
use ftui_web::step_program::{StepProgram, StepResult};
use ftui_web::{WebFlatPatchBatch, WebPatchStats};

/// Platform-independent showcase runner wrapping `StepProgram<AppModel>`.
pub struct RunnerCore {
    inner: StepProgram<AppModel>,
    /// Cached patch hash from the last `take_flat_patches()` call.
    cached_patch_hash: Option<String>,
    /// Cached patch stats from the last `take_flat_patches()` call.
    cached_patch_stats: Option<WebPatchStats>,
    /// Cached logs from the last `take_flat_patches()` call.
    cached_logs: Vec<String>,
}

impl RunnerCore {
    /// Create a new runner with the given initial terminal dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        let model = AppModel::default();
        Self {
            inner: StepProgram::new(model, cols, rows),
            cached_patch_hash: None,
            cached_patch_stats: None,
            cached_logs: Vec::new(),
        }
    }

    /// Initialize the model and render the first frame. Call exactly once.
    pub fn init(&mut self) {
        self.inner
            .init()
            .expect("StepProgram init should not fail on WebBackend");
    }

    /// Advance the deterministic clock by `dt_ms` milliseconds.
    pub fn advance_time_ms(&mut self, dt_ms: f64) {
        let duration = Duration::from_secs_f64(dt_ms / 1000.0);
        self.inner.advance_time(duration);
    }

    /// Set the deterministic clock to absolute nanoseconds.
    pub fn set_time_ns(&mut self, ts_ns: f64) {
        let duration = Duration::from_nanos(ts_ns as u64);
        self.inner.set_time(duration);
    }

    /// Parse a JSON-encoded input event and push to the event queue.
    ///
    /// Returns `true` if the event was accepted, `false` if it was
    /// unsupported, malformed, or had no `Event` mapping.
    pub fn push_encoded_input(&mut self, json: &str) -> bool {
        match ftui_web::input_parser::parse_encoded_input_to_event(json) {
            Ok(Some(event)) => {
                self.inner.push_event(event);
                true
            }
            _ => false,
        }
    }

    /// Resize the terminal. Pushes a `Resize` event processed on the next step.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.inner.resize(cols, rows);
    }

    /// Process pending events and render if dirty.
    pub fn step(&mut self) -> StepResult {
        self.inner
            .step()
            .expect("StepProgram step should not fail on WebBackend")
    }

    /// Take the flat patch batch for GPU upload.
    ///
    /// Also caches patch hash, stats, and logs so they can be read
    /// via `patch_hash()`, `patch_stats()`, and `take_logs()` after
    /// the outputs have been drained.
    pub fn take_flat_patches(&mut self) -> WebFlatPatchBatch {
        let outputs = self.inner.take_outputs();
        self.cached_patch_hash = outputs.last_patch_hash.clone();
        self.cached_patch_stats = outputs.last_patch_stats;
        self.cached_logs = outputs.logs.clone();
        outputs.flatten_patches_u32()
    }

    /// Take accumulated log lines (from the last `take_flat_patches` call).
    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.cached_logs)
    }

    /// FNV-1a hash of the last patch batch.
    pub fn patch_hash(&self) -> Option<String> {
        self.cached_patch_hash.clone()
    }

    /// Patch upload stats.
    pub fn patch_stats(&self) -> Option<WebPatchStats> {
        self.cached_patch_stats
    }

    /// Current frame index (monotonic, 0-based).
    pub fn frame_idx(&self) -> u64 {
        self.inner.frame_idx()
    }

    /// Whether the program is still running.
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }
}
