#![forbid(unsafe_code)]

//! `wasm-bindgen` exports for the ShowcaseRunner.
//!
//! This module wraps [`super::runner_core::RunnerCore`] with JS-friendly types.
//! Only compiled on `wasm32` targets.

use js_sys::{Array, Object, Reflect, Uint32Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::runner_core::RunnerCore;

fn console_error(msg: &str) {
    let global = js_sys::global();
    let Ok(console) = Reflect::get(&global, &"console".into()) else {
        return;
    };
    let Ok(error) = Reflect::get(&console, &"error".into()) else {
        return;
    };
    let Ok(error_fn) = error.dyn_into::<js_sys::Function>() else {
        return;
    };
    let _ = error_fn.call1(&console, &JsValue::from_str(msg));
}

fn install_panic_hook() {
    use std::sync::Once;

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            // Keep it simple and robust: always print something useful.
            let msg = if let Some(loc) = info.location() {
                format!(
                    "panic at {}:{}:{}: {info}",
                    loc.file(),
                    loc.line(),
                    loc.column()
                )
            } else {
                format!("panic: {info}")
            };
            console_error(&msg);
        }));
    });
}

/// WASM showcase runner for the FrankenTUI demo application.
///
/// Host-driven: JavaScript controls the event loop via `requestAnimationFrame`,
/// pushing input events and advancing time each frame.
#[wasm_bindgen]
pub struct ShowcaseRunner {
    inner: RunnerCore,
}

#[wasm_bindgen(start)]
pub fn wasm_start() {
    install_panic_hook();
}

#[wasm_bindgen]
impl ShowcaseRunner {
    /// Create a new runner with initial terminal dimensions (cols, rows).
    #[wasm_bindgen(constructor)]
    pub fn new(cols: u16, rows: u16) -> Self {
        install_panic_hook();
        Self {
            inner: RunnerCore::new(cols, rows),
        }
    }

    /// Provide the Shakespeare text blob for the `Shakespeare` screen.
    ///
    /// For WASM builds we avoid embedding multi-megabyte strings in the module.
    /// The host should call this once during startup (or early in the session).
    #[wasm_bindgen(js_name = setShakespeareText)]
    pub fn set_shakespeare_text(&mut self, text: String) -> bool {
        ftui_demo_showcase::assets::set_shakespeare_text(text)
    }

    /// Provide the SQLite amalgamation source for the `CodeExplorer` screen.
    ///
    /// For WASM builds we avoid embedding multi-megabyte strings in the module.
    /// The host should call this once during startup (or early in the session).
    #[wasm_bindgen(js_name = setSqliteSource)]
    pub fn set_sqlite_source(&mut self, text: String) -> bool {
        ftui_demo_showcase::assets::set_sqlite_source(text)
    }

    /// Initialize the model and render the first frame. Call exactly once.
    pub fn init(&mut self) {
        self.inner.init();
    }

    /// Advance deterministic clock by `dt_ms` milliseconds (real-time mode).
    #[wasm_bindgen(js_name = advanceTime)]
    pub fn advance_time(&mut self, dt_ms: f64) {
        self.inner.advance_time_ms(dt_ms);
    }

    /// Set deterministic clock to absolute nanoseconds (replay mode).
    #[wasm_bindgen(js_name = setTime)]
    pub fn set_time(&mut self, ts_ns: f64) {
        self.inner.set_time_ns(ts_ns);
    }

    /// Parse a JSON-encoded input and push to the event queue.
    /// Returns `true` if accepted, `false` if unsupported/malformed.
    #[wasm_bindgen(js_name = pushEncodedInput)]
    pub fn push_encoded_input(&mut self, json: &str) -> bool {
        self.inner.push_encoded_input(json)
    }

    /// Resize the terminal (pushes Resize event, processed on next step).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.inner.resize(cols, rows);
    }

    /// Process pending events and render if dirty.
    /// Returns `{ running, rendered, events_processed, frame_idx }`.
    pub fn step(&mut self) -> JsValue {
        let result = self.inner.step();
        let obj = Object::new();
        let _ = Reflect::set(&obj, &"running".into(), &result.running.into());
        let _ = Reflect::set(&obj, &"rendered".into(), &result.rendered.into());
        let _ = Reflect::set(
            &obj,
            &"events_processed".into(),
            &result.events_processed.into(),
        );
        let _ = Reflect::set(
            &obj,
            &"frame_idx".into(),
            &JsValue::from_f64(result.frame_idx as f64),
        );
        obj.into()
    }

    /// Take flat patch batch for GPU upload.
    /// Returns `{ cells: Uint32Array, spans: Uint32Array }`.
    #[wasm_bindgen(js_name = takeFlatPatches)]
    pub fn take_flat_patches(&mut self) -> JsValue {
        let flat = self.inner.take_flat_patches();
        let cells = Uint32Array::from(flat.cells.as_slice());
        let spans = Uint32Array::from(flat.spans.as_slice());
        let obj = Object::new();
        let _ = Reflect::set(&obj, &"cells".into(), &cells.into());
        let _ = Reflect::set(&obj, &"spans".into(), &spans.into());
        obj.into()
    }

    /// Drain accumulated log lines. Returns `Array<string>`.
    #[wasm_bindgen(js_name = takeLogs)]
    pub fn take_logs(&mut self) -> Array {
        let logs = self.inner.take_logs();
        let arr = Array::new();
        for log in logs {
            arr.push(&JsValue::from_str(&log));
        }
        arr
    }

    /// FNV-1a hash of the last patch batch, or `null`.
    #[wasm_bindgen(js_name = patchHash)]
    pub fn patch_hash(&self) -> Option<String> {
        self.inner.patch_hash()
    }

    /// Patch upload stats: `{ dirty_cells, patch_count, bytes_uploaded }`, or `null`.
    #[wasm_bindgen(js_name = patchStats)]
    pub fn patch_stats(&self) -> JsValue {
        match self.inner.patch_stats() {
            Some(stats) => {
                let obj = Object::new();
                let _ = Reflect::set(&obj, &"dirty_cells".into(), &stats.dirty_cells.into());
                let _ = Reflect::set(&obj, &"patch_count".into(), &stats.patch_count.into());
                let _ = Reflect::set(
                    &obj,
                    &"bytes_uploaded".into(),
                    &JsValue::from_f64(stats.bytes_uploaded as f64),
                );
                obj.into()
            }
            None => JsValue::NULL,
        }
    }

    /// Current frame index (monotonic, 0-based).
    #[wasm_bindgen(js_name = frameIdx)]
    pub fn frame_idx(&self) -> u64 {
        self.inner.frame_idx()
    }

    /// Whether the program is still running.
    #[wasm_bindgen(js_name = isRunning)]
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Release internal resources.
    pub fn destroy(&mut self) {
        // Currently a no-op — all resources are Drop-cleaned.
        // Placeholder for future WebGPU resource cleanup.
    }
}
