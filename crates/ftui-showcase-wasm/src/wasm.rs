#![forbid(unsafe_code)]

//! `wasm-bindgen` exports for the ShowcaseRunner.
//!
//! This module wraps [`super::runner_core::RunnerCore`] with JS-friendly types.
//! Only compiled on `wasm32` targets.

use js_sys::{Array, Object, Reflect, Uint32Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::runner_core::{
    PaneDispatchOutcome, PaneDispatchSummary, PanePreviewState, PaneTimelineStatus, RunnerCore,
};
use ftui_layout::{
    PaneId, PaneLayoutIntelligenceMode, PaneModifierSnapshot, PanePointerButton, PaneResizeTarget,
    SplitAxis,
};
use ftui_web::pane_pointer_capture::{PanePointerCaptureCommand, PanePointerIgnoredReason};

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

fn set_js(obj: &Object, key: &str, value: JsValue) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &value);
}

fn pane_axis_from_u8(axis: u8) -> Option<SplitAxis> {
    match axis {
        0 => Some(SplitAxis::Horizontal),
        1 => Some(SplitAxis::Vertical),
        _ => None,
    }
}

fn pane_button_from_u8(button: u8) -> Option<PanePointerButton> {
    match button {
        0 => Some(PanePointerButton::Primary),
        1 => Some(PanePointerButton::Secondary),
        2 => Some(PanePointerButton::Middle),
        _ => None,
    }
}

fn pane_modifiers_from_bits(mods: u8) -> PaneModifierSnapshot {
    PaneModifierSnapshot {
        shift: mods & 0b0001 != 0,
        alt: mods & 0b0010 != 0,
        ctrl: mods & 0b0100 != 0,
        meta: mods & 0b1000 != 0,
    }
}

fn pane_mode_from_u8(mode: u8) -> Option<PaneLayoutIntelligenceMode> {
    match mode {
        0 => Some(PaneLayoutIntelligenceMode::Focus),
        1 => Some(PaneLayoutIntelligenceMode::Compare),
        2 => Some(PaneLayoutIntelligenceMode::Monitor),
        3 => Some(PaneLayoutIntelligenceMode::Compact),
        _ => None,
    }
}

fn pane_zone_label(zone: ftui_layout::PaneDockZone) -> &'static str {
    match zone {
        ftui_layout::PaneDockZone::Left => "left",
        ftui_layout::PaneDockZone::Right => "right",
        ftui_layout::PaneDockZone::Top => "top",
        ftui_layout::PaneDockZone::Bottom => "bottom",
        ftui_layout::PaneDockZone::Center => "center",
    }
}

fn pane_rect_to_js(rect: ftui_layout::Rect) -> JsValue {
    let ghost = Object::new();
    set_js(&ghost, "x", JsValue::from_f64(f64::from(rect.x)));
    set_js(&ghost, "y", JsValue::from_f64(f64::from(rect.y)));
    set_js(&ghost, "width", JsValue::from_f64(f64::from(rect.width)));
    set_js(&ghost, "height", JsValue::from_f64(f64::from(rect.height)));
    ghost.into()
}

fn pane_push_dock_candidate(
    candidates: &Array,
    target: Option<PaneId>,
    zone: Option<ftui_layout::PaneDockZone>,
    ghost_rect: Option<ftui_layout::Rect>,
    strength_bps: u16,
) {
    let (Some(target), Some(zone), Some(ghost_rect)) = (target, zone, ghost_rect) else {
        return;
    };
    let candidate = Object::new();
    set_js(&candidate, "target", JsValue::from_f64(target.get() as f64));
    set_js(&candidate, "zone", JsValue::from_str(pane_zone_label(zone)));
    set_js(
        &candidate,
        "dock_strength_bps",
        JsValue::from_f64(f64::from(strength_bps)),
    );
    set_js(&candidate, "ghost_rect", pane_rect_to_js(ghost_rect));
    candidates.push(&candidate.into());
}

fn ignored_reason_label(reason: PanePointerIgnoredReason) -> &'static str {
    match reason {
        PanePointerIgnoredReason::InvalidPointerId => "invalid_pointer_id",
        PanePointerIgnoredReason::ButtonNotAllowed => "button_not_allowed",
        PanePointerIgnoredReason::ButtonMismatch => "button_mismatch",
        PanePointerIgnoredReason::ActivePointerAlreadyInProgress => {
            "active_pointer_already_in_progress"
        }
        PanePointerIgnoredReason::NoActivePointer => "no_active_pointer",
        PanePointerIgnoredReason::PointerMismatch => "pointer_mismatch",
        PanePointerIgnoredReason::LeaveWhileCaptured => "leave_while_captured",
        PanePointerIgnoredReason::MachineRejectedEvent => "machine_rejected_event",
    }
}

fn pane_dispatch_to_js(
    dispatch: PaneDispatchSummary,
    active_pointer_id: Option<u32>,
    preview: PanePreviewState,
    timeline: PaneTimelineStatus,
    layout_hash: u64,
    selected_ids: &[u64],
    primary_id: Option<u64>,
    error: Option<&str>,
) -> JsValue {
    let obj = Object::new();
    set_js(&obj, "accepted", dispatch.accepted().into());
    let phase = match dispatch.phase {
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerDown => "pointer_down",
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerMove => "pointer_move",
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerUp => "pointer_up",
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerCancel => {
            "pointer_cancel"
        }
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerLeave => "pointer_leave",
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::Blur => "blur",
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::VisibilityHidden => {
            "visibility_hidden"
        }
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::LostPointerCapture => {
            "lost_pointer_capture"
        }
        ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::CaptureAcquired => {
            "capture_acquired"
        }
    };
    set_js(&obj, "phase", JsValue::from_str(phase));

    if let Some(sequence) = dispatch.sequence {
        set_js(&obj, "sequence", JsValue::from_f64(sequence as f64));
    } else {
        set_js(&obj, "sequence", JsValue::NULL);
    }

    if let Some(pointer_id) = dispatch.pointer_id {
        set_js(&obj, "pointer_id", JsValue::from_f64(pointer_id as f64));
    } else {
        set_js(&obj, "pointer_id", JsValue::NULL);
    }

    match dispatch.target {
        Some(target) => {
            set_js(
                &obj,
                "split_id",
                JsValue::from_f64(target.split_id.get() as f64),
            );
            let axis = match target.axis {
                SplitAxis::Horizontal => "horizontal",
                SplitAxis::Vertical => "vertical",
            };
            set_js(&obj, "axis", JsValue::from_str(axis));
        }
        None => {
            set_js(&obj, "split_id", JsValue::NULL);
            set_js(&obj, "axis", JsValue::NULL);
        }
    }

    match dispatch.capture_command {
        Some(PanePointerCaptureCommand::Acquire { pointer_id }) => {
            let command = Object::new();
            set_js(&command, "kind", JsValue::from_str("acquire"));
            set_js(&command, "pointer_id", JsValue::from_f64(pointer_id as f64));
            set_js(&obj, "capture_command", command.into());
        }
        Some(PanePointerCaptureCommand::Release { pointer_id }) => {
            let command = Object::new();
            set_js(&command, "kind", JsValue::from_str("release"));
            set_js(&command, "pointer_id", JsValue::from_f64(pointer_id as f64));
            set_js(&obj, "capture_command", command.into());
        }
        None => {
            set_js(&obj, "capture_command", JsValue::NULL);
        }
    }

    match dispatch.outcome {
        PaneDispatchOutcome::SemanticForwarded => {
            set_js(&obj, "outcome", JsValue::from_str("semantic_forwarded"));
            set_js(&obj, "ignored_reason", JsValue::NULL);
        }
        PaneDispatchOutcome::CaptureStateUpdated => {
            set_js(&obj, "outcome", JsValue::from_str("capture_state_updated"));
            set_js(&obj, "ignored_reason", JsValue::NULL);
        }
        PaneDispatchOutcome::Ignored(reason) => {
            set_js(&obj, "outcome", JsValue::from_str("ignored"));
            set_js(
                &obj,
                "ignored_reason",
                JsValue::from_str(ignored_reason_label(reason)),
            );
        }
    }

    if let Some(active_pointer_id) = active_pointer_id {
        set_js(
            &obj,
            "active_pointer_id",
            JsValue::from_f64(active_pointer_id as f64),
        );
    } else {
        set_js(&obj, "active_pointer_id", JsValue::NULL);
    }

    if let Some(error) = error {
        set_js(&obj, "error", JsValue::from_str(error));
    } else {
        set_js(&obj, "error", JsValue::NULL);
    }

    if let Some(source) = preview.source {
        set_js(&obj, "drag_source", JsValue::from_f64(source.get() as f64));
    } else {
        set_js(&obj, "drag_source", JsValue::NULL);
    }
    if let Some(target) = preview.target {
        set_js(&obj, "dock_target", JsValue::from_f64(target.get() as f64));
    } else {
        set_js(&obj, "dock_target", JsValue::NULL);
    }
    if let Some(zone) = preview.zone {
        set_js(&obj, "dock_zone", JsValue::from_str(pane_zone_label(zone)));
    } else {
        set_js(&obj, "dock_zone", JsValue::NULL);
    }
    if let Some(rect) = preview.ghost_rect {
        set_js(&obj, "ghost_rect", pane_rect_to_js(rect));
    } else {
        set_js(&obj, "ghost_rect", JsValue::NULL);
    }
    if let Some(rect) = preview.selection_bounds {
        set_js(&obj, "selection_bounds", pane_rect_to_js(rect));
    } else {
        set_js(&obj, "selection_bounds", JsValue::NULL);
    }
    set_js(
        &obj,
        "dock_strength_bps",
        JsValue::from_f64(f64::from(preview.dock_strength_bps)),
    );
    set_js(
        &obj,
        "motion_speed_cps",
        JsValue::from_f64(f64::from(preview.motion_speed_cps)),
    );
    let dock_candidates = Array::new();
    pane_push_dock_candidate(
        &dock_candidates,
        preview.target,
        preview.zone,
        preview.ghost_rect,
        preview.dock_strength_bps,
    );
    pane_push_dock_candidate(
        &dock_candidates,
        preview.alt_one_target,
        preview.alt_one_zone,
        preview.alt_one_ghost_rect,
        preview.alt_one_strength_bps,
    );
    pane_push_dock_candidate(
        &dock_candidates,
        preview.alt_two_target,
        preview.alt_two_zone,
        preview.alt_two_ghost_rect,
        preview.alt_two_strength_bps,
    );
    set_js(&obj, "dock_candidates", dock_candidates.into());
    set_js(
        &obj,
        "timeline_cursor",
        JsValue::from_f64(timeline.cursor as f64),
    );
    set_js(
        &obj,
        "timeline_length",
        JsValue::from_f64(timeline.len as f64),
    );
    set_js(
        &obj,
        "layout_hash",
        JsValue::from_str(&layout_hash.to_string()),
    );
    let selected = Array::new();
    for pane_id in selected_ids {
        selected.push(&JsValue::from_f64(*pane_id as f64));
    }
    set_js(&obj, "selected_ids", selected.into());
    if let Some(primary_id) = primary_id {
        set_js(&obj, "primary_id", JsValue::from_f64(primary_id as f64));
    } else {
        set_js(&obj, "primary_id", JsValue::NULL);
    }

    obj.into()
}

fn pane_state_to_js(runner: &RunnerCore) -> JsValue {
    let obj = Object::new();
    let preview = runner.pane_preview_state();
    let timeline = runner.pane_timeline_status();
    set_js(
        &obj,
        "timeline_cursor",
        JsValue::from_f64(timeline.cursor as f64),
    );
    set_js(
        &obj,
        "timeline_length",
        JsValue::from_f64(timeline.len as f64),
    );
    set_js(
        &obj,
        "layout_hash",
        JsValue::from_str(&runner.pane_layout_hash().to_string()),
    );
    if let Some(source) = preview.source {
        set_js(&obj, "drag_source", JsValue::from_f64(source.get() as f64));
    } else {
        set_js(&obj, "drag_source", JsValue::NULL);
    }
    if let Some(target) = preview.target {
        set_js(&obj, "dock_target", JsValue::from_f64(target.get() as f64));
    } else {
        set_js(&obj, "dock_target", JsValue::NULL);
    }
    if let Some(zone) = preview.zone {
        set_js(&obj, "dock_zone", JsValue::from_str(pane_zone_label(zone)));
    } else {
        set_js(&obj, "dock_zone", JsValue::NULL);
    }
    if let Some(rect) = preview.ghost_rect {
        set_js(&obj, "ghost_rect", pane_rect_to_js(rect));
    } else {
        set_js(&obj, "ghost_rect", JsValue::NULL);
    }
    if let Some(rect) = preview.selection_bounds {
        set_js(&obj, "selection_bounds", pane_rect_to_js(rect));
    } else {
        set_js(&obj, "selection_bounds", JsValue::NULL);
    }
    set_js(
        &obj,
        "dock_strength_bps",
        JsValue::from_f64(f64::from(preview.dock_strength_bps)),
    );
    set_js(
        &obj,
        "motion_speed_cps",
        JsValue::from_f64(f64::from(preview.motion_speed_cps)),
    );
    let dock_candidates = Array::new();
    pane_push_dock_candidate(
        &dock_candidates,
        preview.target,
        preview.zone,
        preview.ghost_rect,
        preview.dock_strength_bps,
    );
    pane_push_dock_candidate(
        &dock_candidates,
        preview.alt_one_target,
        preview.alt_one_zone,
        preview.alt_one_ghost_rect,
        preview.alt_one_strength_bps,
    );
    pane_push_dock_candidate(
        &dock_candidates,
        preview.alt_two_target,
        preview.alt_two_zone,
        preview.alt_two_ghost_rect,
        preview.alt_two_strength_bps,
    );
    set_js(&obj, "dock_candidates", dock_candidates.into());
    if let Some(pointer_id) = runner.pane_active_pointer_id() {
        set_js(
            &obj,
            "active_pointer_id",
            JsValue::from_f64(f64::from(pointer_id)),
        );
    } else {
        set_js(&obj, "active_pointer_id", JsValue::NULL);
    }

    let selected = Array::new();
    for pane_id in runner.pane_selected_ids() {
        selected.push(&JsValue::from_f64(pane_id as f64));
    }
    set_js(&obj, "selected_ids", selected.into());
    if let Some(primary_id) = runner.pane_primary_id() {
        set_js(&obj, "primary_id", JsValue::from_f64(primary_id as f64));
    } else {
        set_js(&obj, "primary_id", JsValue::NULL);
    }

    obj.into()
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
    fn pane_dispatch_with_state(
        &self,
        dispatch: PaneDispatchSummary,
        error: Option<&str>,
    ) -> JsValue {
        let selected = self.inner.pane_selected_ids();
        pane_dispatch_to_js(
            dispatch,
            self.inner.pane_active_pointer_id(),
            self.inner.pane_preview_state(),
            self.inner.pane_timeline_status(),
            self.inner.pane_layout_hash(),
            &selected,
            self.inner.pane_primary_id(),
            error,
        )
    }

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

    /// Pane-specific pointer-down path with direct capture semantics.
    ///
    /// `axis`: `0` = horizontal, `1` = vertical.
    /// `button`: `0` = primary, `1` = secondary, `2` = middle.
    /// `mods` bitmask: `1=shift`, `2=alt`, `4=ctrl`, `8=meta`.
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(js_name = panePointerDown)]
    pub fn pane_pointer_down(
        &mut self,
        split_id: u64,
        axis: u8,
        pointer_id: u32,
        button: u8,
        x: i32,
        y: i32,
        mods: u8,
    ) -> JsValue {
        let axis = match pane_axis_from_u8(axis) {
            Some(axis) => axis,
            None => {
                let dispatch = PaneDispatchSummary {
                    phase: ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerDown,
                    sequence: None,
                    pointer_id: Some(pointer_id),
                    target: None,
                    capture_command: None,
                    outcome: PaneDispatchOutcome::Ignored(
                        PanePointerIgnoredReason::MachineRejectedEvent,
                    ),
                };
                return self.pane_dispatch_with_state(dispatch, Some("invalid_axis"));
            }
        };
        let button = match pane_button_from_u8(button) {
            Some(button) => button,
            None => {
                let dispatch = PaneDispatchSummary {
                    phase: ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerDown,
                    sequence: None,
                    pointer_id: Some(pointer_id),
                    target: None,
                    capture_command: None,
                    outcome: PaneDispatchOutcome::Ignored(
                        PanePointerIgnoredReason::ButtonNotAllowed,
                    ),
                };
                return self.pane_dispatch_with_state(dispatch, Some("invalid_button"));
            }
        };
        let split_id = match PaneId::new(split_id) {
            Ok(split_id) => split_id,
            Err(_) => {
                let dispatch = PaneDispatchSummary {
                    phase: ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerDown,
                    sequence: None,
                    pointer_id: Some(pointer_id),
                    target: None,
                    capture_command: None,
                    outcome: PaneDispatchOutcome::Ignored(
                        PanePointerIgnoredReason::MachineRejectedEvent,
                    ),
                };
                return self.pane_dispatch_with_state(dispatch, Some("invalid_split_id"));
            }
        };
        let target = PaneResizeTarget { split_id, axis };
        let dispatch = self.inner.pane_pointer_down(
            target,
            pointer_id,
            button,
            x,
            y,
            pane_modifiers_from_bits(mods),
        );
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane pointer-down path that auto-detects pane/edge/corner from coordinates.
    #[wasm_bindgen(js_name = panePointerDownAt)]
    pub fn pane_pointer_down_at(
        &mut self,
        pointer_id: u32,
        button: u8,
        x: i32,
        y: i32,
        mods: u8,
    ) -> JsValue {
        let button = match pane_button_from_u8(button) {
            Some(button) => button,
            None => {
                let dispatch = PaneDispatchSummary {
                    phase: ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerDown,
                    sequence: None,
                    pointer_id: Some(pointer_id),
                    target: None,
                    capture_command: None,
                    outcome: PaneDispatchOutcome::Ignored(
                        PanePointerIgnoredReason::ButtonNotAllowed,
                    ),
                };
                return self.pane_dispatch_with_state(dispatch, Some("invalid_button"));
            }
        };
        let dispatch = self.inner.pane_pointer_down_at(
            pointer_id,
            button,
            x,
            y,
            pane_modifiers_from_bits(mods),
        );
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific pointer capture acknowledgement path.
    #[wasm_bindgen(js_name = panePointerCaptureAcquired)]
    pub fn pane_pointer_capture_acquired(&mut self, pointer_id: u32) -> JsValue {
        let dispatch = self.inner.pane_capture_acquired(pointer_id);
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific pointer-move path.
    #[wasm_bindgen(js_name = panePointerMove)]
    pub fn pane_pointer_move(&mut self, pointer_id: u32, x: i32, y: i32, mods: u8) -> JsValue {
        let dispatch =
            self.inner
                .pane_pointer_move(pointer_id, x, y, pane_modifiers_from_bits(mods));
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Auto-targeted pointer move path.
    #[wasm_bindgen(js_name = panePointerMoveAt)]
    pub fn pane_pointer_move_at(&mut self, pointer_id: u32, x: i32, y: i32, mods: u8) -> JsValue {
        let dispatch =
            self.inner
                .pane_pointer_move_at(pointer_id, x, y, pane_modifiers_from_bits(mods));
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific pointer-up path.
    ///
    /// `button`: `0` = primary, `1` = secondary, `2` = middle.
    /// `mods` bitmask: `1=shift`, `2=alt`, `4=ctrl`, `8=meta`.
    #[wasm_bindgen(js_name = panePointerUp)]
    pub fn pane_pointer_up(
        &mut self,
        pointer_id: u32,
        button: u8,
        x: i32,
        y: i32,
        mods: u8,
    ) -> JsValue {
        let button = match pane_button_from_u8(button) {
            Some(button) => button,
            None => {
                let dispatch = PaneDispatchSummary {
                    phase: ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerUp,
                    sequence: None,
                    pointer_id: Some(pointer_id),
                    target: None,
                    capture_command: None,
                    outcome: PaneDispatchOutcome::Ignored(
                        PanePointerIgnoredReason::ButtonNotAllowed,
                    ),
                };
                return self.pane_dispatch_with_state(dispatch, Some("invalid_button"));
            }
        };
        let dispatch =
            self.inner
                .pane_pointer_up(pointer_id, button, x, y, pane_modifiers_from_bits(mods));
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Auto-targeted pointer-up path.
    #[wasm_bindgen(js_name = panePointerUpAt)]
    pub fn pane_pointer_up_at(
        &mut self,
        pointer_id: u32,
        button: u8,
        x: i32,
        y: i32,
        mods: u8,
    ) -> JsValue {
        let button = match pane_button_from_u8(button) {
            Some(button) => button,
            None => {
                let dispatch = PaneDispatchSummary {
                    phase: ftui_web::pane_pointer_capture::PanePointerLifecyclePhase::PointerUp,
                    sequence: None,
                    pointer_id: Some(pointer_id),
                    target: None,
                    capture_command: None,
                    outcome: PaneDispatchOutcome::Ignored(
                        PanePointerIgnoredReason::ButtonNotAllowed,
                    ),
                };
                return self.pane_dispatch_with_state(dispatch, Some("invalid_button"));
            }
        };
        let dispatch =
            self.inner
                .pane_pointer_up_at(pointer_id, button, x, y, pane_modifiers_from_bits(mods));
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific pointer-cancel path.
    ///
    /// Pass `0` to represent an unspecified pointer id.
    #[wasm_bindgen(js_name = panePointerCancel)]
    pub fn pane_pointer_cancel(&mut self, pointer_id: u32) -> JsValue {
        let pointer_id = (pointer_id != 0).then_some(pointer_id);
        let dispatch = self.inner.pane_pointer_cancel(pointer_id);
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific pointer-leave path.
    #[wasm_bindgen(js_name = panePointerLeave)]
    pub fn pane_pointer_leave(&mut self, pointer_id: u32) -> JsValue {
        let dispatch = self.inner.pane_pointer_leave(pointer_id);
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific blur path.
    #[wasm_bindgen(js_name = paneBlur)]
    pub fn pane_blur(&mut self) -> JsValue {
        let dispatch = self.inner.pane_blur();
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific hidden visibility path.
    #[wasm_bindgen(js_name = paneVisibilityHidden)]
    pub fn pane_visibility_hidden(&mut self) -> JsValue {
        let dispatch = self.inner.pane_visibility_hidden();
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Pane-specific lost pointer capture path.
    #[wasm_bindgen(js_name = paneLostPointerCapture)]
    pub fn pane_lost_pointer_capture(&mut self, pointer_id: u32) -> JsValue {
        let dispatch = self.inner.pane_lost_pointer_capture(pointer_id);
        self.pane_dispatch_with_state(dispatch, None)
    }

    /// Active pane pointer id tracked by the adapter, or `null`.
    #[wasm_bindgen(js_name = paneActivePointerId)]
    pub fn pane_active_pointer_id(&self) -> Option<u32> {
        self.inner.pane_active_pointer_id()
    }

    /// Live pane layout state (ghost preview + timeline + selection).
    #[wasm_bindgen(js_name = paneLayoutState)]
    pub fn pane_layout_state(&self) -> JsValue {
        pane_state_to_js(&self.inner)
    }

    /// Undo one pane structural change.
    #[wasm_bindgen(js_name = paneUndoLayout)]
    pub fn pane_undo_layout(&mut self) -> bool {
        self.inner.pane_undo()
    }

    /// Redo one pane structural change.
    #[wasm_bindgen(js_name = paneRedoLayout)]
    pub fn pane_redo_layout(&mut self) -> bool {
        self.inner.pane_redo()
    }

    /// Rebuild pane tree from timeline baseline and cursor.
    #[wasm_bindgen(js_name = paneReplayLayout)]
    pub fn pane_replay_layout(&mut self) -> bool {
        self.inner.pane_replay()
    }

    /// Export current pane workspace snapshot JSON.
    #[wasm_bindgen(js_name = paneExportWorkspaceSnapshot)]
    pub fn pane_export_workspace_snapshot(&self) -> Option<String> {
        match self.inner.export_workspace_snapshot_json() {
            Ok(json) => Some(json),
            Err(err) => {
                console_error(&format!("paneExportWorkspaceSnapshot failed: {err}"));
                None
            }
        }
    }

    /// Import pane workspace snapshot JSON.
    #[wasm_bindgen(js_name = paneImportWorkspaceSnapshot)]
    pub fn pane_import_workspace_snapshot(&mut self, json: &str) -> bool {
        if let Err(err) = self.inner.import_workspace_snapshot_json(json) {
            console_error(&format!("paneImportWorkspaceSnapshot failed: {err}"));
            return false;
        }
        true
    }

    /// Apply one adaptive pane layout intelligence mode.
    ///
    /// `mode`: `0=focus`, `1=compare`, `2=monitor`, `3=compact`.
    /// `primary_pane_id`: pass `0` to use current selection anchor.
    #[wasm_bindgen(js_name = paneApplyLayoutMode)]
    pub fn pane_apply_layout_mode(&mut self, mode: u8, primary_pane_id: u64) -> bool {
        let Some(mode) = pane_mode_from_u8(mode) else {
            console_error("paneApplyLayoutMode received invalid mode");
            return false;
        };
        let primary_raw = if primary_pane_id == 0 {
            self.inner.pane_primary_id().unwrap_or(1)
        } else {
            primary_pane_id
        };
        let Ok(primary) = PaneId::new(primary_raw) else {
            console_error("paneApplyLayoutMode received invalid primary pane id");
            return false;
        };
        self.inner.pane_apply_intelligence_mode(mode, primary)
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
    ///
    /// Uses reusable internal buffers to avoid per-frame Vec allocation.
    #[wasm_bindgen(js_name = takeFlatPatches)]
    pub fn take_flat_patches(&mut self) -> JsValue {
        self.inner.prepare_flat_patches();
        let cells = Uint32Array::from(self.inner.flat_cells());
        let spans = Uint32Array::from(self.inner.flat_spans());
        let obj = Object::new();
        let _ = Reflect::set(&obj, &"cells".into(), &cells.into());
        let _ = Reflect::set(&obj, &"spans".into(), &spans.into());
        obj.into()
    }

    /// Prepare flat patch buffers in reusable Rust-owned storage.
    ///
    /// Pair this with `flatCellsPtr/flatCellsLen/flatSpansPtr/flatSpansLen`
    /// for a zero-copy JS view over WASM memory.
    #[wasm_bindgen(js_name = prepareFlatPatches)]
    pub fn prepare_flat_patches(&mut self) {
        self.inner.prepare_flat_patches();
    }

    /// Byte-offset pointer to the prepared flat cell payload (`u32` words).
    #[wasm_bindgen(js_name = flatCellsPtr)]
    pub fn flat_cells_ptr(&self) -> u32 {
        let cells = self.inner.flat_cells();
        if cells.is_empty() {
            0
        } else {
            cells.as_ptr() as usize as u32
        }
    }

    /// Length (in `u32` words) of the prepared flat cell payload.
    #[wasm_bindgen(js_name = flatCellsLen)]
    pub fn flat_cells_len(&self) -> u32 {
        self.inner.flat_cells().len().min(u32::MAX as usize) as u32
    }

    /// Byte-offset pointer to the prepared flat span payload (`u32` words).
    #[wasm_bindgen(js_name = flatSpansPtr)]
    pub fn flat_spans_ptr(&self) -> u32 {
        let spans = self.inner.flat_spans();
        if spans.is_empty() {
            0
        } else {
            spans.as_ptr() as usize as u32
        }
    }

    /// Length (in `u32` words) of the prepared flat span payload.
    #[wasm_bindgen(js_name = flatSpansLen)]
    pub fn flat_spans_len(&self) -> u32 {
        self.inner.flat_spans().len().min(u32::MAX as usize) as u32
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
    pub fn patch_hash(&mut self) -> Option<String> {
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
