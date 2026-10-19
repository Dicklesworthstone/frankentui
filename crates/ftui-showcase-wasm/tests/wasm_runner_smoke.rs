#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

//! `wasm-bindgen-test` smoke suite for `ShowcaseRunner`.
//!
//! Exercises the JS-facing `ShowcaseRunner` contract in `wasm32`:
//! initialization, full-span first frame, resize full repaint, JSON input
//! admission, deterministic `patchHash()` under `setTime`, and pane pointer
//! roundtrip.

use ftui_showcase_wasm::ShowcaseRunner;
use js_sys::{Reflect, Uint32Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{console_log, wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_node_experimental);

fn get_prop(obj: &JsValue, key: &str) -> JsValue {
    Reflect::get(obj, &JsValue::from_str(key)).expect("property lookup should succeed")
}

fn log_patch_stats(runner: &ShowcaseRunner, label: &str) {
    let stats = runner.patch_stats();
    if !stats.is_null() && !stats.is_undefined() {
        let dirty = get_prop(&stats, "dirty_cells").as_f64().unwrap_or(0.0) as u64;
        let count = get_prop(&stats, "patch_count").as_f64().unwrap_or(0.0) as u64;
        let bytes = get_prop(&stats, "bytes_uploaded").as_f64().unwrap_or(0.0) as u64;
        console_log!(
            "PATCH_STATS label={} frame={} dirty_cells={} patch_count={} bytes_uploaded={}",
            label,
            runner.frame_idx(),
            dirty,
            count,
            bytes
        );
    }
}

#[wasm_bindgen_test]
fn runner_init_emits_full_span_first_frame() {
    let mut runner = ShowcaseRunner::new(80, 24);
    runner.init();
    assert!(runner.is_running());
    assert_eq!(runner.frame_idx(), 1);
    log_patch_stats(&runner, "init");

    let patches = runner.take_flat_patches();
    let spans: Uint32Array = get_prop(&patches, "spans").dyn_into().unwrap();
    let cells: Uint32Array = get_prop(&patches, "cells").dyn_into().unwrap();
    assert_eq!(spans.to_vec(), vec![0, 1920]);
    assert_eq!(cells.length(), 1920 * 4);
}

#[wasm_bindgen_test]
fn resize_emits_full_span() {
    let mut runner = ShowcaseRunner::new(80, 24);
    runner.init();
    let _ = runner.take_flat_patches();

    assert!(runner.resize(100, 30));
    let step = runner.step();
    assert_eq!(get_prop(&step, "rendered").as_bool(), Some(true));
    log_patch_stats(&runner, "resize");

    let patches = runner.take_flat_patches();
    let spans: Uint32Array = get_prop(&patches, "spans").dyn_into().unwrap();
    let cells: Uint32Array = get_prop(&patches, "cells").dyn_into().unwrap();
    assert_eq!(spans.to_vec(), vec![0, 3000]);
    assert_eq!(cells.length(), 3000 * 4);
}

#[wasm_bindgen_test]
fn push_encoded_input_accepts_contract_json() {
    let mut runner = ShowcaseRunner::new(80, 24);
    runner.init();
    let _ = runner.take_flat_patches();
    let initial_frame = runner.frame_idx();

    assert!(!runner.push_encoded_input("{not valid json"));
    assert!(runner.push_encoded_input(
        r#"{"kind":"key","phase":"down","code":"Tab","mods":0,"repeat":false}"#
    ));
    let step = runner.step();
    assert_eq!(get_prop(&step, "rendered").as_bool(), Some(true));
    assert!(runner.frame_idx() > initial_frame);
    log_patch_stats(&runner, "tab_input");

    let patches = runner.take_flat_patches();
    let spans: Uint32Array = get_prop(&patches, "spans").dyn_into().unwrap();
    assert!(spans.length() >= 2);
}

#[wasm_bindgen_test]
fn deterministic_patch_hash_under_set_time() {
    let mut a = ShowcaseRunner::new(80, 24);
    let mut b = ShowcaseRunner::new(80, 24);
    a.set_time(0.0);
    b.set_time(0.0);
    a.init();
    b.init();
    assert_eq!(a.patch_hash(), b.patch_hash());

    for i in 1..=30u32 {
        let ts = f64::from(i) * 16_000_000.0;
        a.set_time(ts);
        b.set_time(ts);
        if i % 5 == 0 {
            let input = r#"{"kind":"key","phase":"down","code":"Tab","mods":0,"repeat":false}"#;
            assert!(a.push_encoded_input(input));
            assert!(b.push_encoded_input(input));
        }
        let _ = a.step();
        let _ = b.step();
        log_patch_stats(&a, "determinism");
        assert_eq!(
            a.patch_hash(),
            b.patch_hash(),
            "patch_hash diverged at iteration {i}"
        );
    }
}

#[wasm_bindgen_test]
fn pane_pointer_roundtrip_smoke() {
    let mut runner = ShowcaseRunner::new(120, 40);
    runner.init();
    assert!(runner.goto_screen_selector("layout_lab"));
    let _ = runner.step();

    let down = runner.pane_pointer_down_at(1, 0, 10, 10, 0);
    assert!(!down.is_undefined() && !down.is_null());
    let mv = runner.pane_pointer_move_at(1, 14, 10, 0);
    assert!(!mv.is_undefined() && !mv.is_null());
    let up = runner.pane_pointer_up_at(1, 0, 14, 10, 0);
    assert!(!up.is_undefined() && !up.is_null());

    let state = runner.pane_layout_state();
    assert!(!state.is_undefined() && !state.is_null());
    assert!(get_prop(&state, "layout_hash").as_string().is_some());
}
