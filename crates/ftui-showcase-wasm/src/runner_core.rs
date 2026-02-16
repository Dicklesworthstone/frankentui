#![forbid(unsafe_code)]

//! Platform-independent runner core wrapping `StepProgram<AppModel>`.
//!
//! This module contains the logic shared between the wasm-bindgen exports
//! and the native test harness. No JS/WASM types here.

use core::time::Duration;

use ftui_demo_showcase::app::AppModel;
use ftui_layout::{
    PANE_EDGE_GRIP_INSET_CELLS, PANE_MAGNETIC_FIELD_CELLS, PaneDockZone, PaneDragResizeEffect,
    PaneId, PaneInteractionTimeline, PaneLayoutIntelligenceMode, PaneLeaf, PaneModifierSnapshot,
    PaneMotionVector, PaneNodeKind, PaneOperation, PanePlacement, PanePointerButton,
    PanePointerPosition, PanePressureSnapProfile, PaneResizeGrip, PaneResizeTarget,
    PaneSelectionState, PaneSplitRatio, PaneTree, Rect, Sides, SplitAxis, WorkspaceMetadata,
    WorkspaceSnapshot,
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
const DEFAULT_PANE_MARGIN_CELLS: u16 = 1;
const DEFAULT_PANE_PADDING_CELLS: u16 = 1;
const DEFAULT_SPRING_BLEND_BPS: u16 = 3_500;

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneGestureMode {
    Move,
    Resize(PaneResizeGrip),
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActivePaneGesture {
    pointer_id: u32,
    leaf: PaneId,
    mode: PaneGestureMode,
}

/// Live preview metadata consumed by WASM/host renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanePreviewState {
    pub source: Option<PaneId>,
    pub target: Option<PaneId>,
    pub zone: Option<PaneDockZone>,
    pub ghost_rect: Option<Rect>,
}

/// Lightweight timeline status for host HUD updates.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneTimelineStatus {
    pub cursor: usize,
    pub len: usize,
}

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
    /// Create a new runner with the given initial terminal dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        let model = AppModel::default();
        let layout_tree = default_layout_tree();
        Self {
            inner: StepProgram::new(model, cols, rows),
            cached_patch_hash: None,
            cached_patch_stats: None,
            cached_logs: Vec::new(),
            flat_cells_buf: Vec::new(),
            flat_spans_buf: Vec::new(),
            pane_adapter: PanePointerCaptureAdapter::new(PanePointerCaptureConfig::default())
                .expect("default pane pointer adapter config should be valid"),
            pane_logs: Vec::new(),
            timeline: PaneInteractionTimeline::with_baseline(&layout_tree),
            layout_tree,
            selection: PaneSelectionState::default(),
            active_gesture: None,
            preview_state: PanePreviewState::default(),
            next_operation_id: 1,
            intelligence_mode: PaneLayoutIntelligenceMode::Focus,
            workspace_generation: 0,
        }
    }

    /// Initialize the model and render the first frame. Call exactly once.
    pub fn init(&mut self) {
        self.inner
            .init()
            .expect("StepProgram init should not fail on WebBackend");
        self.refresh_cached_patch_meta_from_live_outputs();
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
        self.preview_state = PanePreviewState::default();
    }

    /// Process pending events and render if dirty.
    pub fn step(&mut self) -> StepResult {
        let result = self
            .inner
            .step()
            .expect("StepProgram step should not fail on WebBackend");
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
        if self.timeline.baseline.is_none() {
            self.timeline = PaneInteractionTimeline::with_baseline(&self.layout_tree);
        }
        self.selection = PaneSelectionState::default();
        if let Some(anchor) = snapshot.active_pane_id {
            self.selection.anchor = Some(anchor);
            let _ = self.selection.selected.insert(anchor);
        }
        self.preview_state = PanePreviewState::default();
        self.active_gesture = None;
        self.workspace_generation = snapshot.metadata.saved_generation;
        Ok(())
    }

    /// Undo one pane structural mutation from the timeline.
    pub fn pane_undo(&mut self) -> bool {
        match self.timeline.undo(&mut self.layout_tree) {
            Ok(changed) => {
                if changed {
                    self.workspace_generation = self.workspace_generation.saturating_add(1);
                    self.preview_state = PanePreviewState::default();
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
                    self.preview_state = PanePreviewState::default();
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
                self.preview_state = PanePreviewState::default();
                self.active_gesture = None;
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
        self.intelligence_mode = mode;
        let pressure = PanePressureSnapProfile {
            strength_bps: 8_000,
            hysteresis_bps: 320,
        };
        let applied = self.apply_operations_with_timeline(0, &operations, pressure, true);
        applied > 0
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
        let target = self
            .prepare_auto_pointer_down(pointer_id, pointer, modifiers)
            .unwrap_or(PaneResizeTarget {
                split_id: self.layout_tree.root(),
                axis: SplitAxis::Horizontal,
            });
        self.pane_pointer_down(target, pointer_id, button, x, y, modifiers)
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
        if self.active_gesture.is_none() {
            let pointer = PanePointerPosition::new(x, y);
            if let Some(leaf) = self.leaf_at_pointer(pointer) {
                self.active_gesture = Some(ActivePaneGesture {
                    pointer_id,
                    leaf,
                    mode: PaneGestureMode::Move,
                });
                if !modifiers.shift {
                    self.set_single_selection(leaf);
                }
            }
        }
        let dispatch = self.pane_adapter.pointer_down(
            target,
            pointer_id,
            button,
            PanePointerPosition::new(x, y),
            modifiers,
        );
        self.record_pane_dispatch(dispatch)
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

    fn decorated_pane_rect(rect: Rect) -> Rect {
        let margin = rect.inner(Sides::all(DEFAULT_PANE_MARGIN_CELLS));
        let padded = margin.inner(Sides::all(DEFAULT_PANE_PADDING_CELLS));
        if padded.width == 0 || padded.height == 0 {
            margin
        } else {
            padded
        }
    }

    fn leaf_at_pointer(&self, pointer: PanePointerPosition) -> Option<PaneId> {
        let x = u16::try_from(pointer.x).ok()?;
        let y = u16::try_from(pointer.y).ok()?;
        let layout = self.layout_tree.solve_layout(self.viewport_rect()).ok()?;
        let mut best: Option<(PaneId, u32)> = None;
        for (node_id, rect) in layout.iter() {
            let Some(node) = self.layout_tree.node(node_id) else {
                continue;
            };
            if !matches!(node.kind, PaneNodeKind::Leaf(_)) {
                continue;
            }
            let visual = Self::decorated_pane_rect(rect);
            if !visual.contains(x, y) {
                continue;
            }
            let area = u32::from(visual.width) * u32::from(visual.height);
            match best {
                Some((_, best_area)) if best_area <= area => {}
                _ => best = Some((node_id, area)),
            }
        }
        best.map(|(node_id, _)| node_id)
    }

    fn nearest_axis_split_for_node(&self, node: PaneId, axis: SplitAxis) -> Option<PaneId> {
        let mut cursor = Some(node);
        while let Some(node_id) = cursor {
            let parent_id = self.layout_tree.node(node_id)?.parent?;
            let parent = self.layout_tree.node(parent_id)?;
            if let PaneNodeKind::Split(split) = &parent.kind
                && split.axis == axis
            {
                return Some(parent_id);
            }
            cursor = Some(parent_id);
        }
        None
    }

    fn nearest_split_for_node(&self, node: PaneId) -> Option<PaneId> {
        let mut cursor = Some(node);
        while let Some(node_id) = cursor {
            let parent_id = self.layout_tree.node(node_id)?.parent?;
            let parent = self.layout_tree.node(parent_id)?;
            if matches!(parent.kind, PaneNodeKind::Split(_)) {
                return Some(parent_id);
            }
            cursor = Some(parent_id);
        }
        None
    }

    fn set_single_selection(&mut self, pane_id: PaneId) {
        self.selection.selected.clear();
        let _ = self.selection.selected.insert(pane_id);
        self.selection.anchor = Some(pane_id);
    }

    fn grip_primary_axis(grip: PaneResizeGrip) -> SplitAxis {
        match grip {
            PaneResizeGrip::Left
            | PaneResizeGrip::Right
            | PaneResizeGrip::TopLeft
            | PaneResizeGrip::TopRight
            | PaneResizeGrip::BottomLeft
            | PaneResizeGrip::BottomRight => SplitAxis::Horizontal,
            PaneResizeGrip::Top | PaneResizeGrip::Bottom => SplitAxis::Vertical,
        }
    }

    fn prepare_auto_pointer_down(
        &mut self,
        pointer_id: u32,
        pointer: PanePointerPosition,
        modifiers: PaneModifierSnapshot,
    ) -> Option<PaneResizeTarget> {
        let layout = self.layout_tree.solve_layout(self.viewport_rect()).ok()?;
        let leaf = self.leaf_at_pointer(pointer)?;

        if modifiers.shift {
            self.selection.shift_toggle(leaf);
            if self.selection.anchor.is_none() {
                self.selection.anchor = Some(leaf);
            }
        } else if !(self.selection.selected.contains(&leaf) && self.selection.selected.len() > 1) {
            self.set_single_selection(leaf);
        }

        let grip = layout.classify_resize_grip(leaf, pointer, PANE_EDGE_GRIP_INSET_CELLS);
        let mode = grip.map_or(PaneGestureMode::Move, PaneGestureMode::Resize);
        let axis = grip.map_or(SplitAxis::Horizontal, Self::grip_primary_axis);
        let split_id = self
            .nearest_axis_split_for_node(leaf, axis)
            .or_else(|| self.nearest_split_for_node(leaf))
            .unwrap_or(self.layout_tree.root());

        self.active_gesture = Some(ActivePaneGesture {
            pointer_id,
            leaf,
            mode,
        });
        self.preview_state = PanePreviewState::default();

        Some(PaneResizeTarget { split_id, axis })
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
                self.preview_state = PanePreviewState::default();
            }
            PaneDragResizeEffect::Canceled { .. } => {
                self.active_gesture = None;
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
        let Ok(layout) = self.layout_tree.solve_layout(self.viewport_rect()) else {
            return;
        };

        match active.mode {
            PaneGestureMode::Resize(grip) => {
                if self.selection.selected.len() > 1
                    && self.selection.selected.contains(&active.leaf)
                {
                    let Ok(plan) = self.layout_tree.plan_group_resize(
                        &self.selection,
                        &layout,
                        grip,
                        pointer,
                        pressure,
                    ) else {
                        return;
                    };
                    let _ = self.apply_operations_with_timeline(
                        sequence,
                        &plan.operations,
                        pressure,
                        true,
                    );
                } else {
                    let Ok(plan) = self.layout_tree.plan_edge_resize(
                        active.leaf,
                        &layout,
                        grip,
                        pointer,
                        pressure,
                    ) else {
                        return;
                    };
                    let _ = self.apply_operations_with_timeline(
                        sequence,
                        &plan.operations,
                        pressure,
                        true,
                    );
                }
            }
            PaneGestureMode::Move => {
                let projected_pointer = if committed {
                    dispatch.projected_position.unwrap_or(pointer)
                } else {
                    pointer
                };
                let inertial = committed.then_some(dispatch.inertial_throw).flatten();

                if self.selection.selected.len() > 1
                    && self.selection.selected.contains(&active.leaf)
                {
                    let anchor = self.selection.anchor.unwrap_or(active.leaf);
                    if let Ok(preview_plan) = self.layout_tree.plan_reflow_move_with_preview(
                        anchor,
                        &layout,
                        pointer,
                        motion,
                        inertial,
                        PANE_MAGNETIC_FIELD_CELLS,
                    ) {
                        self.preview_state = PanePreviewState {
                            source: Some(anchor),
                            target: Some(preview_plan.preview.target),
                            zone: Some(preview_plan.preview.zone),
                            ghost_rect: Some(preview_plan.preview.ghost_rect),
                        };
                    }
                    if committed {
                        let Ok(plan) = self.layout_tree.plan_group_move(
                            &self.selection,
                            &layout,
                            projected_pointer,
                            motion,
                            inertial,
                            PANE_MAGNETIC_FIELD_CELLS,
                        ) else {
                            return;
                        };
                        let _ = self.apply_operations_with_timeline(
                            sequence,
                            &plan.operations,
                            pressure,
                            false,
                        );
                    }
                } else {
                    let Ok(plan) = self.layout_tree.plan_reflow_move_with_preview(
                        active.leaf,
                        &layout,
                        pointer,
                        motion,
                        inertial,
                        PANE_MAGNETIC_FIELD_CELLS,
                    ) else {
                        return;
                    };
                    self.preview_state = PanePreviewState {
                        source: Some(active.leaf),
                        target: Some(plan.preview.target),
                        zone: Some(plan.preview.zone),
                        ghost_rect: Some(plan.preview.ghost_rect),
                    };
                    if committed {
                        let _ = self.apply_operations_with_timeline(
                            sequence,
                            &plan.operations,
                            pressure,
                            false,
                        );
                    }
                }
            }
        }
    }

    fn apply_operations_with_timeline(
        &mut self,
        sequence: u64,
        operations: &[PaneOperation],
        pressure: PanePressureSnapProfile,
        spring_blend: bool,
    ) -> usize {
        let mut applied = 0usize;
        for operation in operations {
            let operation = self.spring_adjust_operation(operation.clone(), pressure, spring_blend);
            let operation_id = self.next_operation_id();
            if self
                .timeline
                .apply_and_record(&mut self.layout_tree, sequence, operation_id, operation)
                .is_ok()
            {
                applied = applied.saturating_add(1);
            }
        }
        if applied > 0 {
            self.workspace_generation = self.workspace_generation.saturating_add(1);
        }
        applied
    }

    fn spring_adjust_operation(
        &self,
        operation: PaneOperation,
        pressure: PanePressureSnapProfile,
        spring_blend: bool,
    ) -> PaneOperation {
        if !spring_blend {
            return operation;
        }
        let PaneOperation::SetSplitRatio { split, ratio } = operation else {
            return operation;
        };
        let Some(node) = self.layout_tree.node(split) else {
            return PaneOperation::SetSplitRatio { split, ratio };
        };
        let PaneNodeKind::Split(split_node) = &node.kind else {
            return PaneOperation::SetSplitRatio { split, ratio };
        };
        let current = ratio_to_bps(split_node.ratio);
        let target = ratio_to_bps(ratio);
        let spring_bps = (u32::from(DEFAULT_SPRING_BLEND_BPS)
            + (u32::from(pressure.strength_bps) / 3))
            .clamp(1_500, 9_000) as u16;
        let blended = blend_bps(current, target, spring_bps);
        let denominator = 10_000_u32.saturating_sub(u32::from(blended)).max(1);
        let ratio = PaneSplitRatio::new(u32::from(blended.max(1)), denominator).unwrap_or(ratio);
        PaneOperation::SetSplitRatio { split, ratio }
    }

    fn next_operation_id(&mut self) -> u64 {
        let operation_id = self.next_operation_id;
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        operation_id
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
}

fn leaf_id_for_key(tree: &PaneTree, key: &str) -> Option<PaneId> {
    tree.nodes().find_map(|node| match &node.kind {
        PaneNodeKind::Leaf(leaf) if leaf.surface_key == key => Some(node.id),
        _ => None,
    })
}

fn default_layout_tree() -> PaneTree {
    let mut tree = PaneTree::singleton("pane-1");
    let ratio = PaneSplitRatio::new(1, 1).expect("1:1 split ratio must be valid");
    let root_leaf = tree.root();
    let _ = tree
        .apply_operation(
            1,
            PaneOperation::SplitLeaf {
                target: root_leaf,
                axis: SplitAxis::Horizontal,
                ratio,
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("pane-2"),
            },
        )
        .expect("default layout root split should succeed");

    if let Some(left_leaf) = leaf_id_for_key(&tree, "pane-1") {
        let _ = tree
            .apply_operation(
                2,
                PaneOperation::SplitLeaf {
                    target: left_leaf,
                    axis: SplitAxis::Vertical,
                    ratio,
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("pane-3"),
                },
            )
            .expect("default layout left split should succeed");
    }

    if let Some(right_leaf) = leaf_id_for_key(&tree, "pane-2") {
        let _ = tree
            .apply_operation(
                3,
                PaneOperation::SplitLeaf {
                    target: right_leaf,
                    axis: SplitAxis::Vertical,
                    ratio,
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new("pane-4"),
                },
            )
            .expect("default layout right split should succeed");
    }

    tree
}

fn ratio_to_bps(ratio: PaneSplitRatio) -> u16 {
    let numerator = ratio.numerator() as f64;
    let denominator = ratio.denominator() as f64;
    let total = (numerator + denominator).max(1.0);
    ((numerator / total) * 10_000.0).round().clamp(1.0, 9_999.0) as u16
}

fn blend_bps(current: u16, target: u16, blend_factor_bps: u16) -> u16 {
    let blend = u32::from(blend_factor_bps.clamp(1, 10_000));
    let current = i32::from(current);
    let target = i32::from(target);
    let delta = target.saturating_sub(current);
    let blended = current + ((delta * i32::try_from(blend).unwrap_or(10_000)) / 10_000);
    blended.clamp(1, 9_999) as u16
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
