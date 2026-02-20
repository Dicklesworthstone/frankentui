#![forbid(unsafe_code)]

//! Platform-independent runner core wrapping `StepProgram<AppModel>`.
//!
//! This module contains the logic shared between the wasm-bindgen exports
//! and the native test harness. No JS/WASM types here.

use core::time::Duration;

use ftui_demo_showcase::app::AppModel;
use ftui_demo_showcase::pane_interaction::{
    ActivePaneGesture, PaneAutoPointerDownContext, PaneDragSemanticsContext,
    PaneDragSemanticsInput, PaneGestureArmState, PaneGestureMode, PanePreviewState,
    PaneTimelineApplyState, PaneTimelineStatus,
    apply_drag_semantics as apply_drag_semantics_shared,
    apply_operations_with_timeline as apply_operations_with_timeline_shared,
    arm_active_gesture as arm_active_gesture_shared, default_pane_layout_tree,
    pointer_down_context_at as shared_pointer_down_context_at,
    rollback_timeline_to_cursor as rollback_timeline_to_cursor_shared,
    update_selection_for_pointer_down,
};
use ftui_layout::{
    PANE_EDGE_GRIP_INSET_CELLS, PANE_MAGNETIC_FIELD_CELLS, PaneDockPreview, PaneDockZone,
    PaneDragResizeEffect, PaneId, PaneInteractionTimeline, PaneLayoutIntelligenceMode,
    PaneModifierSnapshot, PaneMotionVector, PaneNodeKind, PaneOperation, PanePointerButton,
    PanePointerPosition, PanePressureSnapProfile, PaneResizeGrip, PaneResizeTarget,
    PaneSelectionState, PaneTree, Rect, SplitAxis, WorkspaceMetadata, WorkspaceSnapshot,
};
use ftui_web::pane_pointer_capture::{
    PanePointerCaptureAdapter, PanePointerCaptureCommand, PanePointerCaptureConfig,
    PanePointerDispatch, PanePointerIgnoredReason, PanePointerLifecyclePhase, PanePointerLogEntry,
    PanePointerLogOutcome,
};
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
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    /// Reusable cell buffer for flat patch output (avoids per-frame allocation).
    flat_cells_buf: Vec<u32>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    /// Reusable span buffer for flat patch output (avoids per-frame allocation).
    flat_spans_buf: Vec<u32>,
    /// Deterministic pane pointer lifecycle adapter for wasm-hosted pane interactions.
    pane_adapter: PanePointerCaptureAdapter,
    /// Structured pane interaction logs (kept separate from presenter output logs).
    pane_logs: Vec<String>,
    /// Interactive pane topology model used for advanced pane semantics.
    layout_tree: PaneTree,
    /// Persistent structural timeline for undo/redo/replay.
    timeline: PaneInteractionTimeline,
    /// Current multi-pane selection cluster.
    selection: PaneSelectionState,
    /// Active drag gesture context.
    active_gesture: Option<ActivePaneGesture>,
    /// Timeline cursor at gesture start, used to rollback canceled drag mutations.
    gesture_timeline_cursor_start: Option<usize>,
    /// Last applied live-reflow operation signature for dedupe.
    live_reflow_signature: Option<u64>,
    /// Current magnetic docking / ghost preview state.
    preview_state: PanePreviewState,
    /// Deterministic operation id source for pane operations.
    next_operation_id: u64,
    /// Active adaptive intelligence mode.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    intelligence_mode: PaneLayoutIntelligenceMode,
    /// Monotonic workspace generation used in persisted snapshots.
    workspace_generation: u64,
}

const PATCH_HASH_ALGO: &str = "fnv1a64";
const FNV64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV64_PRIME: u64 = 0x100000001b3;

/// Host-facing outcome category for one pane lifecycle dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDispatchOutcome {
    SemanticForwarded,
    CaptureStateUpdated,
    Ignored(PanePointerIgnoredReason),
}

/// Host-facing summary of one pane lifecycle dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneDispatchSummary {
    pub phase: PanePointerLifecyclePhase,
    pub sequence: Option<u64>,
    pub pointer_id: Option<u32>,
    pub target: Option<PaneResizeTarget>,
    pub capture_command: Option<PanePointerCaptureCommand>,
    pub outcome: PaneDispatchOutcome,
}

impl PaneDispatchSummary {
    #[must_use]
    pub const fn accepted(self) -> bool {
        !matches!(self.outcome, PaneDispatchOutcome::Ignored(_))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl RunnerCore {
    #[inline]
    fn clamp_dimension(value: u16) -> u16 {
        if value < 1 { 1 } else { value }
    }

    #[inline]
    fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
        (Self::clamp_dimension(cols), Self::clamp_dimension(rows))
    }

    fn pane_adapter_with_fallback(
        config: PanePointerCaptureConfig,
    ) -> (PanePointerCaptureAdapter, Option<String>) {
        match PanePointerCaptureAdapter::new(config) {
            Ok(adapter) => (adapter, None),
            Err(err) => {
                let fallback = PanePointerCaptureConfig {
                    drag_threshold: 1,
                    update_hysteresis: 1,
                    activation_button: config.activation_button,
                    cancel_on_leave_without_capture: config.cancel_on_leave_without_capture,
                };
                match PanePointerCaptureAdapter::new(fallback) {
                    Ok(adapter) => (
                        adapter,
                        Some(format!(
                            "pane_pointer_adapter_config_error: {err}; applied fallback thresholds=1/1"
                        )),
                    ),
                    Err(fallback_err) => (
                        PanePointerCaptureAdapter::default(),
                        Some(format!(
                            "pane_pointer_adapter_config_error: {err}; \
                             fallback thresholds=1/1 rejected: {fallback_err}; \
                             using default pane pointer adapter"
                        )),
                    ),
                }
            }
        }
    }

    /// Create a new runner with the given initial terminal dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = Self::clamp_size(cols, rows);
        let model = AppModel::default();
        let layout_tree = default_pane_layout_tree();
        let (pane_adapter, pane_adapter_log) =
            Self::pane_adapter_with_fallback(PanePointerCaptureConfig::default());
        let mut cached_logs = Vec::new();
        if let Some(log) = pane_adapter_log {
            cached_logs.push(log);
        }
        Self {
            inner: StepProgram::new(model, cols, rows),
            cached_patch_hash: None,
            cached_patch_stats: None,
            cached_logs,
            flat_cells_buf: Vec::new(),
            flat_spans_buf: Vec::new(),
            pane_adapter,
            pane_logs: Vec::new(),
            timeline: PaneInteractionTimeline::with_baseline(&layout_tree),
            layout_tree,
            selection: PaneSelectionState::default(),
            active_gesture: None,
            gesture_timeline_cursor_start: None,
            live_reflow_signature: None,
            preview_state: PanePreviewState::default(),
            next_operation_id: 1,
            intelligence_mode: PaneLayoutIntelligenceMode::Focus,
            workspace_generation: 0,
        }
    }

    /// Initialize the model and render the first frame. Call exactly once.
    pub fn init(&mut self) {
        if self.inner.is_initialized() {
            return;
        }
        if let Err(err) = self.inner.init() {
            self.cached_logs.push(format!("runner_init_error: {err}"));
            return;
        }
        self.refresh_cached_patch_meta_from_live_outputs();
    }

    /// Advance the deterministic clock by `dt_ms` milliseconds.
    pub fn advance_time_ms(&mut self, dt_ms: f64) {
        // Host input can be noisy (NaN/inf/negative spikes). Clamp to a safe,
        // finite non-negative duration so frame scheduling never panics.
        if !dt_ms.is_finite() || dt_ms <= 0.0 {
            return;
        }
        let max_secs = Duration::MAX.as_secs_f64();
        let secs = (dt_ms / 1000.0).min(max_secs);
        let duration = Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX);
        self.inner.advance_time(duration);
    }

    /// Set the deterministic clock to absolute nanoseconds.
    pub fn set_time_ns(&mut self, ts_ns: f64) {
        // Be robust to host-provided non-finite/negative timestamps.
        let nanos = if !ts_ns.is_finite() || ts_ns <= 0.0 {
            0
        } else {
            ts_ns.min(u64::MAX as f64) as u64
        };
        let duration = Duration::from_nanos(nanos);
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
        let (cols, rows) = Self::clamp_size(cols, rows);
        self.inner.resize(cols, rows);
        self.preview_state = PanePreviewState::default();
    }

    /// Process pending events and render if dirty.
    pub fn step(&mut self) -> StepResult {
        if !self.inner.is_initialized() {
            self.init();
            if !self.inner.is_initialized() {
                return StepResult {
                    running: false,
                    rendered: false,
                    events_processed: 0,
                    frame_idx: self.inner.frame_idx(),
                };
            }
        }
        let result = match self.inner.step() {
            Ok(result) => result,
            Err(err) => {
                self.cached_logs.push(format!("runner_step_error: {err}"));
                return StepResult {
                    running: self.inner.is_running(),
                    rendered: false,
                    events_processed: 0,
                    frame_idx: self.inner.frame_idx(),
                };
            }
        };
        if result.rendered {
            self.refresh_cached_patch_meta_from_live_outputs();
        }
        result
    }

    /// Take the flat patch batch for GPU upload.
    ///
    /// Also caches patch hash, stats, and logs so they can be read
    /// via `patch_hash()`, `patch_stats()`, and `take_logs()` after
    /// the outputs have been drained.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn take_flat_patches(&mut self) -> WebFlatPatchBatch {
        let mut outputs = self.inner.take_outputs();
        self.cached_patch_hash = outputs.compute_patch_hash().map(str::to_owned);
        self.cached_patch_stats = outputs.last_patch_stats;
        let flat = outputs.flatten_patches_u32();
        self.cached_logs = outputs.logs;
        flat
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    /// Prepare flat patch data into reusable internal buffers.
    ///
    /// Call this instead of [`take_flat_patches`](Self::take_flat_patches)
    /// when you want to avoid per-frame Vec allocation. Access the results
    /// via [`flat_cells`](Self::flat_cells) and [`flat_spans`](Self::flat_spans).
    pub fn prepare_flat_patches(&mut self) {
        // Flatten into reusable buffers before draining outputs.
        self.inner
            .backend_mut()
            .presenter_mut()
            .flatten_patches_into(&mut self.flat_cells_buf, &mut self.flat_spans_buf);

        // Cache metadata, then drain outputs.
        let outputs = self.inner.take_outputs();
        // Hash stays lazy: compute on-demand from `flat_*_buf` only if asked.
        self.cached_patch_hash = None;
        self.cached_patch_stats = outputs.last_patch_stats;
        self.cached_logs = outputs.logs;
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    /// Flat cell payload from the last [`prepare_flat_patches`](Self::prepare_flat_patches) call.
    pub fn flat_cells(&self) -> &[u32] {
        &self.flat_cells_buf
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    /// Flat span payload from the last [`prepare_flat_patches`](Self::prepare_flat_patches) call.
    pub fn flat_spans(&self) -> &[u32] {
        &self.flat_spans_buf
    }

    /// Take accumulated log lines (from the last `take_flat_patches` call).
    pub fn take_logs(&mut self) -> Vec<String> {
        let mut logs = std::mem::take(&mut self.cached_logs);
        logs.append(&mut self.pane_logs);
        logs
    }

    /// FNV-1a hash of the last patch batch.
    pub fn patch_hash(&mut self) -> Option<String> {
        if self.cached_patch_hash.is_none() {
            if !self.flat_spans_buf.is_empty() {
                self.cached_patch_hash =
                    hash_flat_patch_batch(&self.flat_spans_buf, &self.flat_cells_buf);
            } else {
                let outputs = self.inner.backend_mut().presenter_mut().outputs_mut();
                self.cached_patch_hash = outputs.compute_patch_hash().map(str::to_owned);
            }
        }
        self.cached_patch_hash.clone()
    }

    /// Patch upload stats.
    pub fn patch_stats(&self) -> Option<WebPatchStats> {
        self.cached_patch_stats
            .or(self.inner.outputs().last_patch_stats)
    }

    /// Current frame index (monotonic, 0-based).
    pub fn frame_idx(&self) -> u64 {
        self.inner.frame_idx()
    }

    /// Whether the program is still running.
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Active pane pointer ID currently tracked by the adapter.
    pub fn pane_active_pointer_id(&self) -> Option<u32> {
        self.pane_adapter.active_pointer_id()
    }

    /// Current live magnetic-docking ghost preview.
    #[must_use]
    pub const fn pane_preview_state(&self) -> PanePreviewState {
        self.preview_state
    }

    /// Current timeline cursor/length summary.
    #[must_use]
    pub fn pane_timeline_status(&self) -> PaneTimelineStatus {
        PaneTimelineStatus {
            cursor: self.timeline.cursor,
            len: self.timeline.entries.len(),
        }
    }

    /// Selected pane IDs in deterministic order.
    #[must_use]
    pub fn pane_selected_ids(&self) -> Vec<u64> {
        self.selection
            .as_sorted_vec()
            .into_iter()
            .map(PaneId::get)
            .collect()
    }

    /// Deterministic hash of the current pane topology.
    #[must_use]
    pub fn pane_layout_hash(&self) -> u64 {
        self.layout_tree.state_hash()
    }

    /// Primary pane id used as default focus target for adaptive modes.
    #[must_use]
    pub fn pane_primary_id(&self) -> Option<u64> {
        self.selection
            .anchor
            .or_else(|| {
                self.layout_tree
                    .nodes()
                    .find_map(|node| matches!(node.kind, PaneNodeKind::Leaf(_)).then_some(node.id))
            })
            .map(PaneId::get)
    }

    /// Export pane tree + timeline snapshot as canonical JSON.
    pub fn export_workspace_snapshot_json(&self) -> Result<String, String> {
        let mut metadata = WorkspaceMetadata::new("showcase-runner");
        metadata.saved_generation = self.workspace_generation;
        let mut snapshot = WorkspaceSnapshot::new(self.layout_tree.to_snapshot(), metadata);
        snapshot.interaction_timeline = self.timeline.clone();
        if let Some(baseline) = snapshot.interaction_timeline.baseline.as_mut() {
            baseline.canonicalize();
        }
        snapshot.active_pane_id = self.selection.anchor;
        snapshot
            .validate()
            .map_err(|err| format!("workspace snapshot validation failed: {err}"))?;
        serde_json::to_string(&snapshot)
            .map_err(|err| format!("workspace snapshot encode failed: {err}"))
    }

    /// Restore pane tree + timeline from a previously exported JSON snapshot.
    pub fn import_workspace_snapshot_json(&mut self, json: &str) -> Result<(), String> {
        let snapshot: WorkspaceSnapshot = serde_json::from_str(json)
            .map_err(|err| format!("workspace snapshot parse failed: {err}"))?;
        snapshot
            .validate()
            .map_err(|err| format!("workspace snapshot invalid: {err}"))?;
        self.layout_tree = PaneTree::from_snapshot(snapshot.pane_tree.clone())
            .map_err(|err| format!("pane tree restore failed: {err}"))?;
        self.timeline = snapshot.interaction_timeline;
        if let Some(baseline) = self.timeline.baseline.as_mut() {
            baseline.canonicalize();
        }
        if self.timeline.baseline.is_none() {
            self.timeline = PaneInteractionTimeline::with_baseline(&self.layout_tree);
        }
        self.refresh_next_operation_id_from_timeline();
        self.selection = PaneSelectionState::default();
        if let Some(anchor) = snapshot.active_pane_id {
            self.selection.anchor = Some(anchor);
            let _ = self.selection.selected.insert(anchor);
        }
        self.sanitize_selection_to_layout();
        self.clear_transient_pane_interaction_state();
        self.reset_pointer_capture_adapter();
        self.workspace_generation = snapshot.metadata.saved_generation;
        Ok(())
    }

    /// Undo one pane structural mutation from the timeline.
    pub fn pane_undo(&mut self) -> bool {
        match self.timeline.undo(&mut self.layout_tree) {
            Ok(changed) => {
                if changed {
                    self.workspace_generation = self.workspace_generation.saturating_add(1);
                    self.sanitize_selection_to_layout();
                    self.clear_transient_pane_interaction_state();
                    self.reset_pointer_capture_adapter();
                }
                changed
            }
            Err(err) => {
                self.pane_logs
                    .push(format!("pane_timeline undo error: {err}"));
                false
            }
        }
    }

    /// Redo one pane structural mutation from the timeline.
    pub fn pane_redo(&mut self) -> bool {
        match self.timeline.redo(&mut self.layout_tree) {
            Ok(changed) => {
                if changed {
                    self.workspace_generation = self.workspace_generation.saturating_add(1);
                    self.sanitize_selection_to_layout();
                    self.clear_transient_pane_interaction_state();
                    self.reset_pointer_capture_adapter();
                }
                changed
            }
            Err(err) => {
                self.pane_logs
                    .push(format!("pane_timeline redo error: {err}"));
                false
            }
        }
    }

    /// Deterministically rebuild pane topology from timeline baseline + cursor.
    pub fn pane_replay(&mut self) -> bool {
        match self.timeline.replay() {
            Ok(tree) => {
                self.layout_tree = tree;
                self.workspace_generation = self.workspace_generation.saturating_add(1);
                self.refresh_next_operation_id_from_timeline();
                self.sanitize_selection_to_layout();
                self.clear_transient_pane_interaction_state();
                self.reset_pointer_capture_adapter();
                true
            }
            Err(err) => {
                self.pane_logs
                    .push(format!("pane_timeline replay error: {err}"));
                false
            }
        }
    }

    /// Apply one adaptive layout intelligence mode transition.
    pub fn pane_apply_intelligence_mode(
        &mut self,
        mode: PaneLayoutIntelligenceMode,
        primary: PaneId,
    ) -> bool {
        let operations = match self.layout_tree.plan_intelligence_mode(mode, primary) {
            Ok(operations) => operations,
            Err(err) => {
                self.pane_logs
                    .push(format!("pane_intelligence mode_plan_error: {err}"));
                return false;
            }
        };
        let pressure = PanePressureSnapProfile {
            strength_bps: 8_000,
            hysteresis_bps: 320,
        };
        let applied = self.apply_operations_with_timeline(0, &operations, pressure, true);
        if applied > 0 {
            self.intelligence_mode = mode;
            self.sanitize_selection_to_layout();
            self.clear_transient_pane_interaction_state();
            self.reset_pointer_capture_adapter();
            true
        } else {
            false
        }
    }

    /// Auto-targeted pointer-down from host coordinates (no split-id required).
    pub fn pane_pointer_down_at(
        &mut self,
        pointer_id: u32,
        button: PanePointerButton,
        x: i32,
        y: i32,
        modifiers: PaneModifierSnapshot,
    ) -> PaneDispatchSummary {
        let pointer = PanePointerPosition::new(x, y);
        let Some(context) = self.pointer_down_context_at(pointer) else {
            return self.reject_pointer_down(pointer_id, pointer);
        };
        let target = context.target;
        let dispatch = self
            .pane_adapter
            .pointer_down(target, pointer_id, button, pointer, modifiers);
        let summary = self.record_pane_dispatch(dispatch);
        if summary.accepted() && self.active_gesture.is_none() {
            update_selection_for_pointer_down(&mut self.selection, context.leaf, modifiers.shift);
            self.arm_gesture(pointer_id, context.leaf, context.mode);
        }
        summary
    }

    /// Auto-targeted pointer-move from host coordinates.
    pub fn pane_pointer_move_at(
        &mut self,
        pointer_id: u32,
        x: i32,
        y: i32,
        modifiers: PaneModifierSnapshot,
    ) -> PaneDispatchSummary {
        self.pane_pointer_move(pointer_id, x, y, modifiers)
    }

    /// Auto-targeted pointer-up from host coordinates.
    pub fn pane_pointer_up_at(
        &mut self,
        pointer_id: u32,
        button: PanePointerButton,
        x: i32,
        y: i32,
        modifiers: PaneModifierSnapshot,
    ) -> PaneDispatchSummary {
        self.pane_pointer_up(pointer_id, button, x, y, modifiers)
    }

    /// Handle pane pointer-down and emit capture command if needed.
    pub fn pane_pointer_down(
        &mut self,
        target: PaneResizeTarget,
        pointer_id: u32,
        button: PanePointerButton,
        x: i32,
        y: i32,
        modifiers: PaneModifierSnapshot,
    ) -> PaneDispatchSummary {
        let pointer = PanePointerPosition::new(x, y);
        let dispatch = self
            .pane_adapter
            .pointer_down(target, pointer_id, button, pointer, modifiers);
        let summary = self.record_pane_dispatch(dispatch);
        if summary.accepted()
            && self.active_gesture.is_none()
            && let Some(context) = self.pointer_down_context_at(pointer)
        {
            update_selection_for_pointer_down(&mut self.selection, context.leaf, modifiers.shift);
            self.arm_gesture(pointer_id, context.leaf, context.mode);
        }
        summary
    }

    /// Mark pane pointer capture as acquired by the host/browser.
    pub fn pane_capture_acquired(&mut self, pointer_id: u32) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.capture_acquired(pointer_id);
        self.record_pane_dispatch(dispatch)
    }

    /// Handle pane pointer-move updates.
    pub fn pane_pointer_move(
        &mut self,
        pointer_id: u32,
        x: i32,
        y: i32,
        modifiers: PaneModifierSnapshot,
    ) -> PaneDispatchSummary {
        let dispatch =
            self.pane_adapter
                .pointer_move(pointer_id, PanePointerPosition::new(x, y), modifiers);
        self.record_pane_dispatch(dispatch)
    }

    /// Handle pane pointer-up and capture release if needed.
    pub fn pane_pointer_up(
        &mut self,
        pointer_id: u32,
        button: PanePointerButton,
        x: i32,
        y: i32,
        modifiers: PaneModifierSnapshot,
    ) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.pointer_up(
            pointer_id,
            button,
            PanePointerPosition::new(x, y),
            modifiers,
        );
        self.record_pane_dispatch(dispatch)
    }

    /// Handle pane pointer-cancel lifecycle.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pane_pointer_cancel(&mut self, pointer_id: Option<u32>) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.pointer_cancel(pointer_id);
        self.record_pane_dispatch(dispatch)
    }

    /// Handle pane pointer-leave lifecycle.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pane_pointer_leave(&mut self, pointer_id: u32) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.pointer_leave(pointer_id);
        self.record_pane_dispatch(dispatch)
    }

    /// Handle browser blur for pane interaction lifecycle.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pane_blur(&mut self) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.blur();
        self.record_pane_dispatch(dispatch)
    }

    /// Handle hidden-tab visibility transition for pane interaction lifecycle.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pane_visibility_hidden(&mut self) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.visibility_hidden();
        self.record_pane_dispatch(dispatch)
    }

    /// Handle lost-pointer-capture lifecycle signal.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pane_lost_pointer_capture(&mut self, pointer_id: u32) -> PaneDispatchSummary {
        let dispatch = self.pane_adapter.lost_pointer_capture(pointer_id);
        self.record_pane_dispatch(dispatch)
    }

    fn viewport_rect(&self) -> Rect {
        let (width, height) = self.inner.size();
        Rect::new(0, 0, width.max(1), height.max(1))
    }

    fn pointer_down_context_at(
        &self,
        pointer: PanePointerPosition,
    ) -> Option<PaneAutoPointerDownContext> {
        shared_pointer_down_context_at(
            &self.layout_tree,
            self.viewport_rect(),
            pointer,
            PANE_EDGE_GRIP_INSET_CELLS,
        )
    }

    fn apply_pane_dispatch_semantics(&mut self, dispatch: &PanePointerDispatch) {
        let Some(transition) = dispatch.transition.as_ref() else {
            return;
        };
        let sequence = transition.sequence;
        match transition.effect {
            PaneDragResizeEffect::DragStarted { current, .. }
            | PaneDragResizeEffect::DragUpdated { current, .. } => {
                self.apply_drag_semantics(sequence, current, dispatch, false);
            }
            PaneDragResizeEffect::Committed { end, .. } => {
                self.apply_drag_semantics(sequence, end, dispatch, true);
                self.active_gesture = None;
                self.gesture_timeline_cursor_start = None;
                self.live_reflow_signature = None;
                self.preview_state = PanePreviewState::default();
            }
            PaneDragResizeEffect::Canceled { .. } => {
                self.rollback_active_gesture_mutations();
                self.active_gesture = None;
                self.gesture_timeline_cursor_start = None;
                self.live_reflow_signature = None;
                self.preview_state = PanePreviewState::default();
            }
            PaneDragResizeEffect::Noop { .. }
            | PaneDragResizeEffect::Armed { .. }
            | PaneDragResizeEffect::KeyboardApplied { .. }
            | PaneDragResizeEffect::WheelApplied { .. } => {}
        }
    }

    fn apply_drag_semantics(
        &mut self,
        sequence: u64,
        pointer: PanePointerPosition,
        dispatch: &PanePointerDispatch,
        committed: bool,
    ) {
        let Some(active) = self.active_gesture else {
            return;
        };
        if dispatch.log.pointer_id != Some(active.pointer_id) {
            return;
        }
        let pressure = dispatch
            .pressure_snap_profile()
            .unwrap_or(PanePressureSnapProfile {
                strength_bps: 4_000,
                hysteresis_bps: 240,
            });
        let motion = dispatch
            .motion
            .unwrap_or_else(|| PaneMotionVector::from_delta(0, 0, 16, 0));
        let viewport = self.viewport_rect();
        let _ = apply_drag_semantics_shared(
            PaneDragSemanticsContext {
                layout_tree: &mut self.layout_tree,
                timeline: &mut self.timeline,
                next_operation_id: &mut self.next_operation_id,
                workspace_generation: &mut self.workspace_generation,
                selection: &self.selection,
                preview_state: &mut self.preview_state,
                live_reflow_signature: &mut self.live_reflow_signature,
                viewport,
                baseline_magnetic_field_cells: PANE_MAGNETIC_FIELD_CELLS,
            },
            PaneDragSemanticsInput {
                sequence,
                active,
                pointer,
                pressure,
                motion,
                projected_position: dispatch.projected_position,
                inertial_throw: dispatch.inertial_throw,
                committed,
            },
        );
    }

    fn apply_operations_with_timeline(
        &mut self,
        sequence: u64,
        operations: &[PaneOperation],
        pressure: PanePressureSnapProfile,
        spring_blend: bool,
    ) -> usize {
        apply_operations_with_timeline_shared(
            PaneTimelineApplyState {
                layout_tree: &mut self.layout_tree,
                timeline: &mut self.timeline,
                next_operation_id: &mut self.next_operation_id,
                workspace_generation: &mut self.workspace_generation,
            },
            sequence,
            operations,
            pressure,
            spring_blend,
        )
    }

    fn arm_gesture(&mut self, pointer_id: u32, leaf: PaneId, mode: PaneGestureMode) {
        arm_active_gesture_shared(
            PaneGestureArmState {
                active_gesture: &mut self.active_gesture,
                gesture_timeline_cursor_start: &mut self.gesture_timeline_cursor_start,
                live_reflow_signature: &mut self.live_reflow_signature,
                preview_state: &mut self.preview_state,
            },
            self.timeline.cursor,
            pointer_id,
            leaf,
            mode,
        );
    }

    fn rollback_active_gesture_mutations(&mut self) {
        match rollback_timeline_to_cursor_shared(
            &mut self.layout_tree,
            &mut self.timeline,
            self.gesture_timeline_cursor_start,
            &mut self.workspace_generation,
        ) {
            Ok(_) => {}
            Err(err) => {
                self.pane_logs
                    .push(format!("pane_timeline cancel_rollback error: {err}"));
            }
        }
    }

    fn clear_transient_pane_interaction_state(&mut self) {
        self.preview_state = PanePreviewState::default();
        self.active_gesture = None;
        self.gesture_timeline_cursor_start = None;
        self.live_reflow_signature = None;
    }

    fn sanitize_selection_to_layout(&mut self) {
        let valid_leaves: std::collections::BTreeSet<PaneId> = self
            .layout_tree
            .nodes()
            .filter_map(|node| matches!(node.kind, PaneNodeKind::Leaf(_)).then_some(node.id))
            .collect();
        self.selection
            .selected
            .retain(|pane_id| valid_leaves.contains(pane_id));
        if self
            .selection
            .anchor
            .is_some_and(|anchor| !valid_leaves.contains(&anchor))
        {
            self.selection.anchor = None;
        }
        if let Some(anchor) = self.selection.anchor {
            let _ = self.selection.selected.insert(anchor);
        } else {
            self.selection.anchor = self.selection.selected.iter().next().copied();
        }
    }

    fn reset_pointer_capture_adapter(&mut self) {
        let config = self.pane_adapter.config();
        let (adapter, log) = Self::pane_adapter_with_fallback(config);
        self.pane_adapter = adapter;
        if let Some(log) = log {
            self.cached_logs.push(log);
        }
    }

    fn refresh_next_operation_id_from_timeline(&mut self) {
        self.next_operation_id = self
            .timeline
            .entries
            .iter()
            .map(|entry| entry.operation_id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
    }

    fn refresh_cached_patch_meta_from_live_outputs(&mut self) {
        let outputs = self.inner.outputs();
        // Invalidate heavy hash cache on each newly rendered frame. Compute only
        // when explicitly requested by the host.
        self.cached_patch_hash = None;
        self.cached_patch_stats = outputs.last_patch_stats;
        self.flat_cells_buf.clear();
        self.flat_spans_buf.clear();
    }

    fn record_pane_dispatch(&mut self, dispatch: PanePointerDispatch) -> PaneDispatchSummary {
        self.apply_pane_dispatch_semantics(&dispatch);
        let summary = PaneDispatchSummary {
            phase: dispatch.log.phase,
            sequence: dispatch.log.sequence,
            pointer_id: dispatch.log.pointer_id,
            target: dispatch.log.target,
            capture_command: dispatch.capture_command,
            outcome: match dispatch.log.outcome {
                PanePointerLogOutcome::SemanticForwarded => PaneDispatchOutcome::SemanticForwarded,
                PanePointerLogOutcome::CaptureStateUpdated => {
                    PaneDispatchOutcome::CaptureStateUpdated
                }
                PanePointerLogOutcome::Ignored(reason) => PaneDispatchOutcome::Ignored(reason),
            },
        };
        self.pane_logs.push(format_pane_log_entry(dispatch.log));
        summary
    }

    fn reject_pointer_down(
        &mut self,
        pointer_id: u32,
        pointer: PanePointerPosition,
    ) -> PaneDispatchSummary {
        let log = PanePointerLogEntry {
            phase: PanePointerLifecyclePhase::PointerDown,
            sequence: None,
            pointer_id: Some(pointer_id),
            target: None,
            position: Some(pointer),
            capture_command: None,
            outcome: PanePointerLogOutcome::Ignored(PanePointerIgnoredReason::MachineRejectedEvent),
        };
        let summary = PaneDispatchSummary {
            phase: log.phase,
            sequence: log.sequence,
            pointer_id: log.pointer_id,
            target: log.target,
            capture_command: log.capture_command,
            outcome: PaneDispatchOutcome::Ignored(PanePointerIgnoredReason::MachineRejectedEvent),
        };
        self.pane_logs.push(format_pane_log_entry(log));
        summary
    }
}

fn format_split_axis(axis: SplitAxis) -> &'static str {
    match axis {
        SplitAxis::Horizontal => "horizontal",
        SplitAxis::Vertical => "vertical",
    }
}

fn format_capture_command(command: Option<PanePointerCaptureCommand>) -> &'static str {
    match command {
        Some(PanePointerCaptureCommand::Acquire { .. }) => "acquire",
        Some(PanePointerCaptureCommand::Release { .. }) => "release",
        None => "none",
    }
}

fn format_ignored_reason(reason: PanePointerIgnoredReason) -> &'static str {
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

fn format_pane_log_entry(log: PanePointerLogEntry) -> String {
    let phase = match log.phase {
        PanePointerLifecyclePhase::PointerDown => "pointer_down",
        PanePointerLifecyclePhase::PointerMove => "pointer_move",
        PanePointerLifecyclePhase::PointerUp => "pointer_up",
        PanePointerLifecyclePhase::PointerCancel => "pointer_cancel",
        PanePointerLifecyclePhase::PointerLeave => "pointer_leave",
        PanePointerLifecyclePhase::Blur => "blur",
        PanePointerLifecyclePhase::VisibilityHidden => "visibility_hidden",
        PanePointerLifecyclePhase::LostPointerCapture => "lost_pointer_capture",
        PanePointerLifecyclePhase::CaptureAcquired => "capture_acquired",
    };
    let pointer_id = log
        .pointer_id
        .map_or_else(|| "-".to_owned(), |id| id.to_string());
    let sequence = log
        .sequence
        .map_or_else(|| "-".to_owned(), |seq| seq.to_string());
    let (split_id, axis) = match log.target {
        Some(target) => (
            target.split_id.get().to_string(),
            format_split_axis(target.axis).to_owned(),
        ),
        None => ("-".to_owned(), "-".to_owned()),
    };
    let (x, y) = match log.position {
        Some(pos) => (pos.x.to_string(), pos.y.to_string()),
        None => ("-".to_owned(), "-".to_owned()),
    };
    let outcome = match log.outcome {
        PanePointerLogOutcome::SemanticForwarded => "semantic_forwarded".to_owned(),
        PanePointerLogOutcome::CaptureStateUpdated => "capture_state_updated".to_owned(),
        PanePointerLogOutcome::Ignored(reason) => {
            format!("ignored:{}", format_ignored_reason(reason))
        }
    };
    let command = format_capture_command(log.capture_command);
    format!(
        "pane_pointer phase={phase} seq={sequence} pointer={pointer_id} split={split_id} axis={axis} x={x} y={y} command={command} outcome={outcome}"
    )
}

#[must_use]
fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

#[must_use]
fn hash_flat_patch_batch(spans: &[u32], cells: &[u32]) -> Option<String> {
    if spans.is_empty() {
        return None;
    }
    if !spans.len().is_multiple_of(2) {
        return None;
    }

    let mut hash = FNV64_OFFSET_BASIS;
    let patch_count = u64::try_from(spans.len() / 2).unwrap_or(u64::MAX);
    hash = fnv1a64_extend(hash, &patch_count.to_le_bytes());

    let mut word_idx = 0usize;
    let mut cell_bytes = [0u8; 16];
    for span in spans.chunks_exact(2) {
        let offset = span[0];
        let len = span[1] as usize;
        let cell_count = u64::try_from(len).unwrap_or(u64::MAX);
        hash = fnv1a64_extend(hash, &offset.to_le_bytes());
        hash = fnv1a64_extend(hash, &cell_count.to_le_bytes());

        let words_needed = len.saturating_mul(4);
        if word_idx.saturating_add(words_needed) > cells.len() {
            return None;
        }

        for _ in 0..len {
            let bg = cells[word_idx];
            let fg = cells[word_idx + 1];
            let glyph = cells[word_idx + 2];
            let attrs = cells[word_idx + 3];
            word_idx += 4;

            cell_bytes[0..4].copy_from_slice(&bg.to_le_bytes());
            cell_bytes[4..8].copy_from_slice(&fg.to_le_bytes());
            cell_bytes[8..12].copy_from_slice(&glyph.to_le_bytes());
            cell_bytes[12..16].copy_from_slice(&attrs.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell_bytes);
        }
    }

    if word_idx != cells.len() {
        return None;
    }

    Some(format!("{PATCH_HASH_ALGO}:{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_demo_showcase::pane_interaction::{
        adaptive_dock_strength_bps, build_preview_state_from_candidates,
        dynamic_live_reflow_threshold_bps, dynamic_preview_switch_advantage_bps,
        edge_fling_projection,
    };

    #[test]
    fn live_reflow_threshold_drops_for_fast_confident_motion() {
        let slow_noisy = dynamic_live_reflow_threshold_bps(
            PaneMotionVector::from_delta(6, 1, 220, 7),
            PanePressureSnapProfile {
                strength_bps: 2_800,
                hysteresis_bps: 150,
            },
        );
        let fast_stable = dynamic_live_reflow_threshold_bps(
            PaneMotionVector::from_delta(38, 2, 40, 0),
            PanePressureSnapProfile {
                strength_bps: 8_400,
                hysteresis_bps: 540,
            },
        );
        assert!(fast_stable < slow_noisy);
    }

    #[test]
    fn preview_switch_advantage_increases_with_noise() {
        let stable = dynamic_preview_switch_advantage_bps(
            PaneMotionVector::from_delta(26, 2, 62, 0),
            PanePressureSnapProfile {
                strength_bps: 7_600,
                hysteresis_bps: 380,
            },
        );
        let noisy = dynamic_preview_switch_advantage_bps(
            PaneMotionVector::from_delta(26, 2, 62, 8),
            PanePressureSnapProfile {
                strength_bps: 7_600,
                hysteresis_bps: 380,
            },
        );
        assert!(noisy > stable);
    }

    #[test]
    fn adaptive_dock_strength_rewards_fast_confident_commits() {
        let fast = adaptive_dock_strength_bps(
            0.72,
            PaneMotionVector::from_delta(42, 4, 34, 0),
            PanePressureSnapProfile {
                strength_bps: 8_400,
                hysteresis_bps: 520,
            },
            true,
        );
        let precise = adaptive_dock_strength_bps(
            0.72,
            PaneMotionVector::from_delta(6, 1, 240, 7),
            PanePressureSnapProfile {
                strength_bps: 2_800,
                hysteresis_bps: 180,
            },
            false,
        );
        assert!(fast > precise);
    }

    #[test]
    fn preview_state_includes_secondary_dock_candidates() {
        let primary = PaneDockPreview {
            target: PaneId::new(2).expect("valid pane id"),
            zone: PaneDockZone::Right,
            score: 0.88,
            ghost_rect: Rect::new(20, 4, 30, 12),
        };
        let secondary = PaneDockPreview {
            target: PaneId::new(3).expect("valid pane id"),
            zone: PaneDockZone::Bottom,
            score: 0.61,
            ghost_rect: Rect::new(20, 16, 30, 8),
        };
        let tertiary = PaneDockPreview {
            target: PaneId::new(4).expect("valid pane id"),
            zone: PaneDockZone::Center,
            score: 0.54,
            ghost_rect: Rect::new(10, 4, 40, 20),
        };
        let state = build_preview_state_from_candidates(
            PaneId::new(1).expect("valid pane id"),
            primary,
            primary.ghost_rect,
            8_800,
            420,
            &[primary, secondary, tertiary],
            Some(Rect::new(8, 3, 40, 22)),
        );
        assert_eq!(state.alt_one_target, Some(secondary.target));
        assert_eq!(state.alt_two_target, Some(tertiary.target));
        assert!(state.alt_one_strength_bps > state.alt_two_strength_bps);
        assert_eq!(state.selection_bounds, Some(Rect::new(8, 3, 40, 22)));
    }

    #[test]
    fn ignored_pointer_down_does_not_arm_gesture_or_selection() {
        let mut runner = RunnerCore::new(100, 32);
        runner.init();
        let before = runner.selection.clone();
        let summary = runner.pane_pointer_down_at(
            17,
            PanePointerButton::Secondary,
            6,
            6,
            PaneModifierSnapshot::default(),
        );
        assert!(!summary.accepted());
        assert!(runner.active_gesture.is_none());
        assert_eq!(runner.selection, before);
    }

    #[test]
    fn out_of_bounds_pointer_down_is_rejected_without_capture() {
        let mut runner = RunnerCore::new(100, 32);
        runner.init();
        let summary = runner.pane_pointer_down_at(
            19,
            PanePointerButton::Primary,
            -4,
            -2,
            PaneModifierSnapshot::default(),
        );
        assert!(!summary.accepted());
        assert!(matches!(
            summary.outcome,
            PaneDispatchOutcome::Ignored(PanePointerIgnoredReason::MachineRejectedEvent)
        ));
        assert_eq!(runner.pane_active_pointer_id(), None);
        assert!(runner.active_gesture.is_none());
    }

    #[test]
    fn edge_pointer_down_arms_resize_gesture() {
        let mut runner = RunnerCore::new(100, 32);
        runner.init();
        let summary = runner.pane_pointer_down_at(
            23,
            PanePointerButton::Primary,
            0,
            0,
            PaneModifierSnapshot::default(),
        );
        assert!(summary.accepted());
        match runner.active_gesture {
            Some(ActivePaneGesture {
                mode: PaneGestureMode::Resize(PaneResizeGrip::TopLeft),
                ..
            }) => {}
            other => panic!("expected top-left resize gesture, got {other:?}"),
        }
    }

    #[test]
    fn init_is_idempotent() {
        let mut runner = RunnerCore::new(100, 32);
        runner.init();
        runner.init();
        assert!(runner.inner.is_initialized());
        let result = runner.step();
        assert!(result.running);
    }

    #[test]
    fn step_auto_initializes_when_needed() {
        let mut runner = RunnerCore::new(100, 32);
        let result = runner.step();
        assert!(runner.inner.is_initialized());
        assert!(result.running);
        assert!(result.frame_idx >= 1);
    }

    #[test]
    fn edge_fling_projection_pushes_fast_edge_release_outward() {
        let viewport = Rect::new(0, 0, 120, 40);
        let projected = edge_fling_projection(
            PanePointerPosition::new(2, 20),
            PaneMotionVector::from_delta(-36, 0, 42, 0),
            viewport,
        );
        assert!(projected.x < 2);
    }

    #[test]
    fn edge_fling_projection_keeps_slow_motion_unchanged() {
        let viewport = Rect::new(0, 0, 120, 40);
        let pointer = PanePointerPosition::new(2, 20);
        let projected = edge_fling_projection(
            pointer,
            PaneMotionVector::from_delta(-3, 0, 420, 0),
            viewport,
        );
        assert_eq!(projected, pointer);
    }

    #[test]
    fn pane_replay_prunes_stale_selection_ids() {
        let mut runner = RunnerCore::new(100, 32);
        runner.init();
        let stale = PaneId::new(999).expect("nonexistent pane id should be constructible");
        runner.selection.anchor = Some(stale);
        let _ = runner.selection.selected.insert(stale);

        assert!(runner.pane_replay());
        assert!(runner.selection.anchor.is_none());
        assert!(runner.selection.selected.is_empty());
    }

    #[test]
    fn intelligence_mode_apply_prunes_stale_selection_ids() {
        let mut runner = RunnerCore::new(100, 32);
        runner.init();
        let stale = PaneId::new(999).expect("nonexistent pane id should be constructible");
        runner.selection.anchor = Some(stale);
        let _ = runner.selection.selected.insert(stale);
        let primary = runner
            .layout_tree
            .nodes()
            .find_map(|node| matches!(node.kind, PaneNodeKind::Leaf(_)).then_some(node.id))
            .expect("default layout should contain a leaf");

        let applied = [
            PaneLayoutIntelligenceMode::Compare,
            PaneLayoutIntelligenceMode::Monitor,
            PaneLayoutIntelligenceMode::Compact,
            PaneLayoutIntelligenceMode::Focus,
        ]
        .into_iter()
        .any(|mode| runner.pane_apply_intelligence_mode(mode, primary));

        assert!(
            applied,
            "expected at least one intelligence mode to apply operations"
        );
        assert!(runner.selection.anchor.is_none());
        assert!(runner.selection.selected.is_empty());
    }
}
