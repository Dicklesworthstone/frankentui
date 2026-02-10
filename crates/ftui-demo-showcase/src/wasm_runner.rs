#![forbid(unsafe_code)]

//! WASM runner for `ftui-demo-showcase`.
//!
//! This module provides a small `wasm-bindgen` surface so a browser host can:
//! - feed normalized inputs (encoded JSON from `FrankenTermWeb.drainEncodedInputs()`),
//! - step the Elm model with a deterministic clock,
//! - drain flat patch batches compatible with `FrankenTermWeb.applyPatchBatchFlat(...)`.

use core::time::Duration;

use crate::app::AppModel;
use crate::screens;
use ftui_web::WebPatchStats;
use ftui_web::input_parser::parse_encoded_input_to_event;
use ftui_web::step_program::{StepProgram, StepResult};
use js_sys::{Array, Object, Reflect, Uint32Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct ShowcaseRunner {
    program: StepProgram<AppModel>,
    pending_spans: Vec<u32>,
    pending_cells: Vec<u32>,
    pending_logs: Vec<String>,
    pending_patch_hash: Option<String>,
    pending_full_repaint: bool,
    pending_patch_stats: Option<WebPatchStats>,
}

#[wasm_bindgen]
impl ShowcaseRunner {
    /// Create and initialize a host-driven showcase runner.
    ///
    /// - `cols`/`rows`: initial terminal grid size in cells.
    /// - `view`: optional view selector (matches screen title/short label/tags
    ///   after normalization; see `screens::screen_registry()`).
    #[wasm_bindgen(constructor)]
    pub fn new(cols: u16, rows: u16, view: Option<String>) -> Result<Self, JsValue> {
        let cols = cols.max(1);
        let rows = rows.max(1);

        let mut model = AppModel::new();
        if let Some(view) = view.as_deref() {
            if let Some(id) = screen_id_from_view(view) {
                model.current_screen = id;
            }
        }

        let mut program = StepProgram::new(model, cols, rows);
        program
            .init()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let mut runner = Self {
            program,
            pending_spans: Vec::new(),
            pending_cells: Vec::new(),
            pending_logs: Vec::new(),
            pending_patch_hash: None,
            pending_full_repaint: false,
            pending_patch_stats: None,
        };
        runner.capture_outputs();
        Ok(runner)
    }

    /// Resize the runner (pushes an `Event::Resize` into the program).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.program.resize(cols.max(1), rows.max(1));
    }

    /// Apply a batch of encoded input JSON strings (from `FrankenTermWeb.drainEncodedInputs()`).
    ///
    /// Returns the number of `ftui_core::event::Event`s successfully enqueued.
    #[wasm_bindgen(js_name = applyEncodedInputs)]
    pub fn apply_encoded_inputs(&mut self, inputs: Array) -> Result<u32, JsValue> {
        let mut pushed: u32 = 0;
        for value in inputs.iter() {
            let Some(s) = value.as_string() else {
                return Err(JsValue::from_str("encoded inputs must be strings"));
            };

            let ev =
                parse_encoded_input_to_event(&s).map_err(|e| JsValue::from_str(&e.to_string()))?;
            if let Some(ev) = ev {
                self.program.push_event(ev);
                pushed = pushed.saturating_add(1);
            }
        }
        Ok(pushed)
    }

    /// Advance deterministic time by `dt_ms` and process one step.
    ///
    /// Returns a JS object:
    /// `{ running, rendered, eventsProcessed, frameIdx }`.
    pub fn step(&mut self, dt_ms: u32) -> Result<JsValue, JsValue> {
        self.program
            .advance_time(Duration::from_millis(u64::from(dt_ms)));
        let result = self
            .program
            .step()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if result.rendered {
            self.capture_outputs();
        }
        step_result_to_js(result)
    }

    /// Drain the last rendered patch batch as flat `Uint32Array` payloads.
    ///
    /// Returns a JS object:
    /// `{ spans, cells, patchHash, fullRepaintHint, patchStats, logs }`.
    ///
    /// Drain semantics: after this call, the pending batch/logs are cleared.
    #[wasm_bindgen(js_name = drainPatchBatchFlat)]
    pub fn drain_patch_batch_flat(&mut self) -> Result<JsValue, JsValue> {
        let obj = Object::new();

        let spans = Uint32Array::from(self.pending_spans.as_slice());
        let cells = Uint32Array::from(self.pending_cells.as_slice());

        Reflect::set(&obj, &JsValue::from_str("spans"), &spans.into())?;
        Reflect::set(&obj, &JsValue::from_str("cells"), &cells.into())?;

        let hash = self
            .pending_patch_hash
            .as_deref()
            .map(JsValue::from_str)
            .unwrap_or(JsValue::NULL);
        Reflect::set(&obj, &JsValue::from_str("patchHash"), &hash)?;
        Reflect::set(
            &obj,
            &JsValue::from_str("fullRepaintHint"),
            &JsValue::from_bool(self.pending_full_repaint),
        )?;

        let stats = if let Some(stats) = self.pending_patch_stats {
            let stats_obj = Object::new();
            Reflect::set(
                &stats_obj,
                &JsValue::from_str("dirtyCells"),
                &JsValue::from_f64(f64::from(stats.dirty_cells)),
            )?;
            Reflect::set(
                &stats_obj,
                &JsValue::from_str("patchCount"),
                &JsValue::from_f64(f64::from(stats.patch_count)),
            )?;
            Reflect::set(
                &stats_obj,
                &JsValue::from_str("bytesUploaded"),
                &JsValue::from_f64(stats.bytes_uploaded as f64),
            )?;
            stats_obj.into()
        } else {
            JsValue::NULL
        };
        Reflect::set(&obj, &JsValue::from_str("patchStats"), &stats)?;

        let logs = Array::new();
        for line in self.pending_logs.drain(..) {
            logs.push(&JsValue::from_str(&line));
        }
        Reflect::set(&obj, &JsValue::from_str("logs"), &logs.into())?;

        // Clear the drained batch.
        self.pending_spans.clear();
        self.pending_cells.clear();
        self.pending_patch_hash = None;
        self.pending_full_repaint = false;
        self.pending_patch_stats = None;

        Ok(obj.into())
    }

    fn capture_outputs(&mut self) {
        let outputs = self.program.take_outputs();
        let flat = outputs.flatten_patches_u32();

        self.pending_spans = flat.spans;
        self.pending_cells = flat.cells;
        self.pending_logs = outputs.logs;
        self.pending_patch_hash = outputs.last_patch_hash;
        self.pending_full_repaint = outputs.last_full_repaint_hint;
        self.pending_patch_stats = outputs.last_patch_stats;
    }
}

fn normalize_view_key(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_us = false;
    for ch in value.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn screen_id_from_view(view: &str) -> Option<crate::app::ScreenId> {
    let want = normalize_view_key(view);
    if want.is_empty() {
        return None;
    }

    for meta in screens::screen_registry() {
        if normalize_view_key(meta.title) == want {
            return Some(meta.id);
        }
        if normalize_view_key(meta.short_label) == want {
            return Some(meta.id);
        }
        for &tag in meta.palette_tags {
            if normalize_view_key(tag) == want {
                return Some(meta.id);
            }
        }
    }
    None
}

fn step_result_to_js(result: StepResult) -> Result<JsValue, JsValue> {
    let obj = Object::new();
    Reflect::set(
        &obj,
        &JsValue::from_str("running"),
        &JsValue::from_bool(result.running),
    )?;
    Reflect::set(
        &obj,
        &JsValue::from_str("rendered"),
        &JsValue::from_bool(result.rendered),
    )?;
    Reflect::set(
        &obj,
        &JsValue::from_str("eventsProcessed"),
        &JsValue::from_f64(f64::from(result.events_processed)),
    )?;
    Reflect::set(
        &obj,
        &JsValue::from_str("frameIdx"),
        &JsValue::from_f64(result.frame_idx as f64),
    )?;
    Ok(obj.into())
}
