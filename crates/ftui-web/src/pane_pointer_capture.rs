#![forbid(unsafe_code)]

//! Deterministic web pointer-capture adapter for pane drag/resize interactions.
//!
//! This module bridges browser pointer lifecycle signals into
//! [`ftui_layout::PaneSemanticInputEvent`] values while enforcing:
//! - one active pointer at a time,
//! - explicit capture acquire/release commands for JS hosts, and
//! - cancellation on interruption paths (blur/visibility/lost-capture).

use ftui_layout::{
    PANE_DRAG_RESIZE_DEFAULT_HYSTERESIS, PANE_DRAG_RESIZE_DEFAULT_THRESHOLD, PaneCancelReason,
    PaneDragResizeMachine, PaneDragResizeMachineError, PaneDragResizeState,
    PaneDragResizeTransition, PaneInertialThrow, PaneModifierSnapshot, PaneMotionVector,
    PanePointerButton, PanePointerPosition, PanePressureSnapProfile, PaneResizeTarget,
    PaneSemanticInputEvent, PaneSemanticInputEventKind,
};

/// Adapter configuration for pane pointer-capture lifecycle handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanePointerCaptureConfig {
    /// Drag start threshold in pane-local units.
    pub drag_threshold: u16,
    /// Drag update hysteresis threshold in pane-local units.
    pub update_hysteresis: u16,
    /// Button required to begin a drag sequence.
    pub activation_button: PanePointerButton,
    /// If true, pointer leave cancels drag when capture was requested but never acknowledged.
    pub cancel_on_leave_without_capture: bool,
}

impl Default for PanePointerCaptureConfig {
    fn default() -> Self {
        Self {
            drag_threshold: PANE_DRAG_RESIZE_DEFAULT_THRESHOLD,
            update_hysteresis: PANE_DRAG_RESIZE_DEFAULT_HYSTERESIS,
            activation_button: PanePointerButton::Primary,
            cancel_on_leave_without_capture: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Requested,
    Acquired,
}

impl CaptureState {
    const fn is_acquired(self) -> bool {
        matches!(self, Self::Acquired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActivePointerCapture {
    pointer_id: u32,
    target: PaneResizeTarget,
    button: PanePointerButton,
    last_position: PanePointerPosition,
    cumulative_delta_x: i32,
    cumulative_delta_y: i32,
    direction_changes: u16,
    sample_count: u32,
    previous_step_sign_x: i8,
    previous_step_sign_y: i8,
    capture_state: CaptureState,
}

impl ActivePointerCapture {
    fn new(
        pointer_id: u32,
        target: PaneResizeTarget,
        button: PanePointerButton,
        position: PanePointerPosition,
    ) -> Self {
        Self {
            pointer_id,
            target,
            button,
            last_position: position,
            cumulative_delta_x: 0,
            cumulative_delta_y: 0,
            direction_changes: 0,
            sample_count: 0,
            previous_step_sign_x: 0,
            previous_step_sign_y: 0,
            capture_state: CaptureState::Requested,
        }
    }

    const fn delta_sign(delta: i32) -> i8 {
        if delta > 0 {
            1
        } else if delta < 0 {
            -1
        } else {
            0
        }
    }

    /// Fold one pointer sample into the motion accumulators and return the step
    /// delta `(x, y)` computed from `last_position`.
    ///
    /// The returned delta lets the caller build the `PointerMove` semantic event
    /// without re-subtracting the same positions — `last_position` is advanced by
    /// the caller (only on a committed transition), so this delta is exactly the
    /// one the event needs.
    fn record_pointer_step(&mut self, position: PanePointerPosition) -> (i32, i32) {
        let step_delta_x = position.x.saturating_sub(self.last_position.x);
        let step_delta_y = position.y.saturating_sub(self.last_position.y);
        let step_sign_x = Self::delta_sign(step_delta_x);
        let step_sign_y = Self::delta_sign(step_delta_y);

        if self.sample_count > 0
            && ((step_sign_x != 0
                && self.previous_step_sign_x != 0
                && step_sign_x != self.previous_step_sign_x)
                || (step_sign_y != 0
                    && self.previous_step_sign_y != 0
                    && step_sign_y != self.previous_step_sign_y))
        {
            self.direction_changes = self.direction_changes.saturating_add(1);
        }

        self.cumulative_delta_x = self.cumulative_delta_x.saturating_add(step_delta_x);
        self.cumulative_delta_y = self.cumulative_delta_y.saturating_add(step_delta_y);
        self.sample_count = self.sample_count.saturating_add(1);
        self.previous_step_sign_x = step_sign_x;
        self.previous_step_sign_y = step_sign_y;

        (step_delta_x, step_delta_y)
    }

    fn motion_summary(&self) -> PaneMotionVector {
        PaneMotionVector::from_delta(
            self.cumulative_delta_x,
            self.cumulative_delta_y,
            self.sample_count.saturating_mul(16),
            self.direction_changes,
        )
    }

    fn release_command(self) -> Option<PanePointerCaptureCommand> {
        self.capture_state
            .is_acquired()
            .then_some(PanePointerCaptureCommand::Release {
                pointer_id: self.pointer_id,
            })
    }

    fn finish_gesture(
        self,
        position: PanePointerPosition,
    ) -> (PaneMotionVector, PaneInertialThrow, PanePointerPosition) {
        let motion = self.motion_summary();
        let inertial_throw = PaneInertialThrow::from_motion(motion);
        let projected_position = inertial_throw.projected_pointer(position);
        (motion, inertial_throw, projected_position)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DispatchContext {
    phase: PanePointerLifecyclePhase,
    pointer_id: Option<u32>,
    target: Option<PaneResizeTarget>,
    position: Option<PanePointerPosition>,
}

/// Host command emitted by the adapter for browser pointer-capture control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePointerCaptureCommand {
    Acquire { pointer_id: u32 },
    Release { pointer_id: u32 },
}

/// Lifecycle phase recorded for one adapter dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePointerLifecyclePhase {
    PointerDown,
    PointerMove,
    PointerUp,
    PointerCancel,
    PointerLeave,
    NativeTouchGesture,
    Blur,
    VisibilityHidden,
    LostPointerCapture,
    ContextLost,
    RenderStalled,
    CaptureAcquired,
}

/// Deterministic reason why an incoming lifecycle signal was ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePointerIgnoredReason {
    InvalidPointerId,
    ButtonNotAllowed,
    ButtonMismatch,
    ActivePointerAlreadyInProgress,
    NativeTouchGesture,
    NoActivePointer,
    PointerMismatch,
    LeaveWhileCaptured,
    MachineRejectedEvent,
}

/// Outcome category for one lifecycle dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePointerLogOutcome {
    SemanticForwarded,
    CaptureStateUpdated,
    Ignored(PanePointerIgnoredReason),
}

/// Structured lifecycle log record for one adapter dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanePointerLogEntry {
    pub phase: PanePointerLifecyclePhase,
    pub sequence: Option<u64>,
    pub pointer_id: Option<u32>,
    pub target: Option<PaneResizeTarget>,
    pub position: Option<PanePointerPosition>,
    pub capture_command: Option<PanePointerCaptureCommand>,
    pub outcome: PanePointerLogOutcome,
}

/// Result of one pointer lifecycle dispatch.
#[derive(Debug, Clone, PartialEq)]
pub struct PanePointerDispatch {
    pub semantic_event: Option<PaneSemanticInputEvent>,
    pub transition: Option<PaneDragResizeTransition>,
    pub motion: Option<PaneMotionVector>,
    pub inertial_throw: Option<PaneInertialThrow>,
    pub projected_position: Option<PanePointerPosition>,
    pub capture_command: Option<PanePointerCaptureCommand>,
    pub log: PanePointerLogEntry,
}

impl PanePointerDispatch {
    /// Derive dynamic snap profile for this dispatch from captured motion.
    #[must_use]
    pub fn pressure_snap_profile(&self) -> Option<PanePressureSnapProfile> {
        self.motion.map(PanePressureSnapProfile::from_motion)
    }

    fn ignored(
        phase: PanePointerLifecyclePhase,
        reason: PanePointerIgnoredReason,
        pointer_id: Option<u32>,
        target: Option<PaneResizeTarget>,
        position: Option<PanePointerPosition>,
    ) -> Self {
        Self {
            semantic_event: None,
            transition: None,
            motion: None,
            inertial_throw: None,
            projected_position: None,
            capture_command: None,
            log: PanePointerLogEntry {
                phase,
                sequence: None,
                pointer_id,
                target,
                position,
                capture_command: None,
                outcome: PanePointerLogOutcome::Ignored(reason),
            },
        }
    }

    fn capture_state_updated(
        phase: PanePointerLifecyclePhase,
        pointer_id: u32,
        target: PaneResizeTarget,
    ) -> Self {
        Self {
            semantic_event: None,
            transition: None,
            motion: None,
            inertial_throw: None,
            projected_position: None,
            capture_command: None,
            log: PanePointerLogEntry {
                phase,
                sequence: None,
                pointer_id: Some(pointer_id),
                target: Some(target),
                position: None,
                capture_command: None,
                outcome: PanePointerLogOutcome::CaptureStateUpdated,
            },
        }
    }
}

/// Deterministic pointer-capture adapter for pane web hosts.
///
/// The adapter emits semantic events accepted by [`PaneDragResizeMachine`] and
/// returns host pointer-capture commands that can be wired to DOM
/// `setPointerCapture()` / `releasePointerCapture()`.
#[derive(Debug, Clone)]
pub struct PanePointerCaptureAdapter {
    machine: PaneDragResizeMachine,
    config: PanePointerCaptureConfig,
    active: Option<ActivePointerCapture>,
    next_sequence: u64,
}

impl Default for PanePointerCaptureAdapter {
    fn default() -> Self {
        Self {
            machine: PaneDragResizeMachine::default(),
            config: PanePointerCaptureConfig::default(),
            active: None,
            next_sequence: 1,
        }
    }
}

impl PanePointerCaptureAdapter {
    /// Construct a new adapter with validated thresholds.
    pub fn new(config: PanePointerCaptureConfig) -> Result<Self, PaneDragResizeMachineError> {
        let machine = PaneDragResizeMachine::new_with_hysteresis(
            config.drag_threshold,
            config.update_hysteresis,
        )?;
        Ok(Self {
            machine,
            config,
            active: None,
            next_sequence: 1,
        })
    }

    /// Adapter configuration.
    #[must_use]
    pub const fn config(&self) -> PanePointerCaptureConfig {
        self.config
    }

    /// Active pointer ID, if any.
    #[must_use]
    pub fn active_pointer_id(&self) -> Option<u32> {
        self.active.map(|active| active.pointer_id)
    }

    /// Current pane drag/resize state machine state.
    #[must_use]
    pub const fn machine_state(&self) -> PaneDragResizeState {
        self.machine.state()
    }

    #[allow(clippy::result_large_err)]
    fn active_for_pointer(
        &self,
        phase: PanePointerLifecyclePhase,
        pointer_id: u32,
        position: Option<PanePointerPosition>,
    ) -> Result<ActivePointerCapture, PanePointerDispatch> {
        let Some(active) = self.active else {
            return Err(PanePointerDispatch::ignored(
                phase,
                PanePointerIgnoredReason::NoActivePointer,
                Some(pointer_id),
                None,
                position,
            ));
        };
        if active.pointer_id != pointer_id {
            return Err(PanePointerDispatch::ignored(
                phase,
                PanePointerIgnoredReason::PointerMismatch,
                Some(pointer_id),
                Some(active.target),
                position,
            ));
        }
        Ok(active)
    }

    /// Handle pointer-down on a pane splitter target.
    pub fn pointer_down(
        &mut self,
        target: PaneResizeTarget,
        pointer_id: u32,
        button: PanePointerButton,
        position: PanePointerPosition,
        modifiers: PaneModifierSnapshot,
    ) -> PanePointerDispatch {
        if pointer_id == 0 {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerDown,
                PanePointerIgnoredReason::InvalidPointerId,
                Some(pointer_id),
                Some(target),
                Some(position),
            );
        }
        if button != self.config.activation_button {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerDown,
                PanePointerIgnoredReason::ButtonNotAllowed,
                Some(pointer_id),
                Some(target),
                Some(position),
            );
        }
        if self.active.is_some() {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerDown,
                PanePointerIgnoredReason::ActivePointerAlreadyInProgress,
                Some(pointer_id),
                Some(target),
                Some(position),
            );
        }

        let kind = PaneSemanticInputEventKind::PointerDown {
            target,
            pointer_id,
            button,
            position,
        };
        let dispatch = self.forward_semantic(
            DispatchContext {
                phase: PanePointerLifecyclePhase::PointerDown,
                pointer_id: Some(pointer_id),
                target: Some(target),
                position: Some(position),
            },
            kind,
            modifiers,
            Some(PanePointerCaptureCommand::Acquire { pointer_id }),
        );
        if dispatch.transition.is_some() {
            self.active = Some(ActivePointerCapture::new(
                pointer_id, target, button, position,
            ));
        }
        dispatch
    }

    /// Handle touch pointer-down with deterministic multi-touch arbitration.
    ///
    /// A single touch follows the normal primary-button resize path. Any
    /// multi-touch start yields to the host's native/custom scroll or pinch
    /// layer by canceling the active pane capture before the second touch is
    /// allowed to proceed.
    pub fn touch_pointer_down(
        &mut self,
        target: PaneResizeTarget,
        pointer_id: u32,
        position: PanePointerPosition,
        active_touch_points: u8,
        modifiers: PaneModifierSnapshot,
    ) -> PanePointerDispatch {
        if active_touch_points > 1 {
            return self.native_touch_gesture();
        }
        self.pointer_down(
            target,
            pointer_id,
            PanePointerButton::Primary,
            position,
            modifiers,
        )
    }

    /// Yield the current pane gesture to a native/custom touch gesture layer.
    pub fn native_touch_gesture(&mut self) -> PanePointerDispatch {
        let Some(active) = self.active else {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::NativeTouchGesture,
                PanePointerIgnoredReason::NativeTouchGesture,
                None,
                None,
                None,
            );
        };
        self.cancel_active(
            PanePointerLifecyclePhase::NativeTouchGesture,
            Some(active.pointer_id),
            PaneCancelReason::PointerCancel,
            true,
        )
    }

    /// Mark browser pointer capture as successfully acquired.
    pub fn capture_acquired(&mut self, pointer_id: u32) -> PanePointerDispatch {
        let mut active = match self.active_for_pointer(
            PanePointerLifecyclePhase::CaptureAcquired,
            pointer_id,
            None,
        ) {
            Ok(active) => active,
            Err(dispatch) => return dispatch,
        };
        active.capture_state = CaptureState::Acquired;
        self.active = Some(active);
        PanePointerDispatch::capture_state_updated(
            PanePointerLifecyclePhase::CaptureAcquired,
            pointer_id,
            active.target,
        )
    }

    /// Handle pointer-move during an active drag lifecycle.
    pub fn pointer_move(
        &mut self,
        pointer_id: u32,
        position: PanePointerPosition,
        modifiers: PaneModifierSnapshot,
    ) -> PanePointerDispatch {
        let mut active = match self.active_for_pointer(
            PanePointerLifecyclePhase::PointerMove,
            pointer_id,
            Some(position),
        ) {
            Ok(active) => active,
            Err(dispatch) => return dispatch,
        };

        // Fold the sample once and reuse the step delta for the semantic event
        // (record_pointer_step does not advance `last_position`, so this delta is
        // identical to re-subtracting the positions — one subtraction pair, not two).
        let (delta_x, delta_y) = active.record_pointer_step(position);
        let kind = PaneSemanticInputEventKind::PointerMove {
            target: active.target,
            pointer_id,
            position,
            delta_x,
            delta_y,
        };

        let mut dispatch = self.forward_semantic(
            DispatchContext {
                phase: PanePointerLifecyclePhase::PointerMove,
                pointer_id: Some(pointer_id),
                target: Some(active.target),
                position: Some(position),
            },
            kind,
            modifiers,
            None,
        );
        if dispatch.transition.is_some() {
            active.last_position = position;
            self.active = Some(active);
            dispatch.motion = Some(active.motion_summary());
        }
        dispatch
    }

    /// Handle pointer-up and release capture for the active pointer.
    pub fn pointer_up(
        &mut self,
        pointer_id: u32,
        button: PanePointerButton,
        position: PanePointerPosition,
        modifiers: PaneModifierSnapshot,
    ) -> PanePointerDispatch {
        let active = match self.active_for_pointer(
            PanePointerLifecyclePhase::PointerUp,
            pointer_id,
            Some(position),
        ) {
            Ok(active) => active,
            Err(dispatch) => return dispatch,
        };
        if active.button != button {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerUp,
                PanePointerIgnoredReason::ButtonMismatch,
                Some(pointer_id),
                Some(active.target),
                Some(position),
            );
        }

        let kind = PaneSemanticInputEventKind::PointerUp {
            target: active.target,
            pointer_id,
            button: active.button,
            position,
        };
        let mut dispatch = self.forward_semantic(
            DispatchContext {
                phase: PanePointerLifecyclePhase::PointerUp,
                pointer_id: Some(pointer_id),
                target: Some(active.target),
                position: Some(position),
            },
            kind,
            modifiers,
            active.release_command(),
        );
        if dispatch.transition.is_some() {
            let (motion, inertial, projected_position) = active.finish_gesture(position);
            dispatch.motion = Some(motion);
            dispatch.projected_position = Some(projected_position);
            dispatch.inertial_throw = Some(inertial);
            self.active = None;
        }
        dispatch
    }

    /// Handle browser pointer-cancel events.
    pub fn pointer_cancel(&mut self, pointer_id: Option<u32>) -> PanePointerDispatch {
        self.cancel_active(
            PanePointerLifecyclePhase::PointerCancel,
            pointer_id,
            PaneCancelReason::PointerCancel,
            true,
        )
    }

    /// Handle pointer-leave lifecycle events.
    pub fn pointer_leave(&mut self, pointer_id: u32) -> PanePointerDispatch {
        let active = match self.active_for_pointer(
            PanePointerLifecyclePhase::PointerLeave,
            pointer_id,
            None,
        ) {
            Ok(active) => active,
            Err(dispatch) => return dispatch,
        };

        if matches!(active.capture_state, CaptureState::Requested)
            && self.config.cancel_on_leave_without_capture
        {
            self.cancel_active(
                PanePointerLifecyclePhase::PointerLeave,
                Some(pointer_id),
                PaneCancelReason::PointerCancel,
                true,
            )
        } else {
            PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerLeave,
                PanePointerIgnoredReason::LeaveWhileCaptured,
                Some(pointer_id),
                Some(active.target),
                None,
            )
        }
    }

    /// Handle browser blur.
    pub fn blur(&mut self) -> PanePointerDispatch {
        let Some(active) = self.active else {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::Blur,
                PanePointerIgnoredReason::NoActivePointer,
                None,
                None,
                None,
            );
        };
        let kind = PaneSemanticInputEventKind::Blur {
            target: Some(active.target),
        };
        let dispatch = self.forward_semantic(
            DispatchContext {
                phase: PanePointerLifecyclePhase::Blur,
                pointer_id: Some(active.pointer_id),
                target: Some(active.target),
                position: None,
            },
            kind,
            PaneModifierSnapshot::default(),
            active.release_command(),
        );
        if dispatch.transition.is_some() {
            self.active = None;
        }
        dispatch
    }

    /// Handle visibility-hidden interruptions.
    pub fn visibility_hidden(&mut self) -> PanePointerDispatch {
        self.cancel_active(
            PanePointerLifecyclePhase::VisibilityHidden,
            None,
            PaneCancelReason::FocusLost,
            true,
        )
    }

    /// Handle `lostpointercapture`; emits cancel and clears active state.
    pub fn lost_pointer_capture(&mut self, pointer_id: u32) -> PanePointerDispatch {
        self.cancel_active(
            PanePointerLifecyclePhase::LostPointerCapture,
            Some(pointer_id),
            PaneCancelReason::PointerCancel,
            false,
        )
    }

    /// Handle WebGPU or host rendering context loss.
    pub fn context_lost(&mut self) -> PanePointerDispatch {
        self.cancel_active(
            PanePointerLifecyclePhase::ContextLost,
            None,
            PaneCancelReason::ContextLost,
            true,
        )
    }

    /// Handle a host render stall that interrupts an active pane gesture.
    pub fn render_stalled(&mut self) -> PanePointerDispatch {
        self.cancel_active(
            PanePointerLifecyclePhase::RenderStalled,
            None,
            PaneCancelReason::RenderStalled,
            true,
        )
    }

    fn cancel_active(
        &mut self,
        phase: PanePointerLifecyclePhase,
        pointer_id: Option<u32>,
        reason: PaneCancelReason,
        release_capture: bool,
    ) -> PanePointerDispatch {
        let Some(active) = self.active else {
            return PanePointerDispatch::ignored(
                phase,
                PanePointerIgnoredReason::NoActivePointer,
                pointer_id,
                None,
                None,
            );
        };
        if let Some(id) = pointer_id
            && id != active.pointer_id
        {
            return PanePointerDispatch::ignored(
                phase,
                PanePointerIgnoredReason::PointerMismatch,
                Some(id),
                Some(active.target),
                None,
            );
        }

        let kind = PaneSemanticInputEventKind::Cancel {
            target: Some(active.target),
            reason,
        };
        let command = if release_capture {
            active.release_command()
        } else {
            None
        };
        let dispatch = self.forward_semantic(
            DispatchContext {
                phase,
                pointer_id: Some(active.pointer_id),
                target: Some(active.target),
                position: None,
            },
            kind,
            PaneModifierSnapshot::default(),
            command,
        );
        if dispatch.transition.is_some() {
            self.active = None;
        }
        dispatch
    }

    fn forward_semantic(
        &mut self,
        context: DispatchContext,
        kind: PaneSemanticInputEventKind,
        modifiers: PaneModifierSnapshot,
        capture_command: Option<PanePointerCaptureCommand>,
    ) -> PanePointerDispatch {
        let mut event = PaneSemanticInputEvent::new(self.next_sequence(), kind);
        event.modifiers = modifiers;
        match self.machine.apply_event(&event) {
            Ok(transition) => {
                let sequence = Some(event.sequence);
                PanePointerDispatch {
                    semantic_event: Some(event),
                    transition: Some(transition),
                    motion: None,
                    inertial_throw: None,
                    projected_position: None,
                    capture_command,
                    log: PanePointerLogEntry {
                        phase: context.phase,
                        sequence,
                        pointer_id: context.pointer_id,
                        target: context.target,
                        position: context.position,
                        capture_command,
                        outcome: PanePointerLogOutcome::SemanticForwarded,
                    },
                }
            }
            Err(_error) => PanePointerDispatch::ignored(
                context.phase,
                PanePointerIgnoredReason::MachineRejectedEvent,
                context.pointer_id,
                context.target,
                context.position,
            ),
        }
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        sequence
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PanePointerCaptureAdapter, PanePointerCaptureCommand, PanePointerCaptureConfig,
        PanePointerDispatch, PanePointerIgnoredReason, PanePointerLifecyclePhase,
        PanePointerLogOutcome,
    };
    use ftui_layout::{
        PANE_TREE_SCHEMA_VERSION, PaneCancelReason, PaneDragResizeEffect, PaneDragResizeState,
        PaneId, PaneInertialThrow, PaneLayout, PaneLeaf, PaneModifierSnapshot, PaneMotionVector,
        PaneNodeKind, PaneNodeRecord, PanePointerButton, PanePointerPosition,
        PanePressureSnapProfile, PaneResizeTarget, PaneSemanticInputEventKind, PaneSplit,
        PaneSplitRatio, PaneTree, PaneTreeSnapshot, Rect, SplitAxis,
    };

    fn target() -> PaneResizeTarget {
        PaneResizeTarget {
            split_id: PaneId::MIN,
            axis: SplitAxis::Horizontal,
        }
    }

    fn pos(x: i32, y: i32) -> PanePointerPosition {
        PanePointerPosition::new(x, y)
    }

    fn adapter() -> PanePointerCaptureAdapter {
        PanePointerCaptureAdapter::new(PanePointerCaptureConfig::default())
            .expect("default config should be valid")
    }

    fn pane_id(raw: u64) -> PaneId {
        PaneId::new(raw).expect("test pane id must be non-zero")
    }

    /// A horizontal root split (left | right-column) where the right child is a
    /// vertical split, matching the runtime bridge test's fixture. The root
    /// split spans the whole viewport regardless of ratio, so one solve targets
    /// it precisely.
    fn nested_pane_tree() -> PaneTree {
        let root = pane_id(1);
        let left = pane_id(2);
        let right_split = pane_id(3);
        let right_top = pane_id(4);
        let right_bottom = pane_id(5);
        let snapshot = PaneTreeSnapshot {
            schema_version: PANE_TREE_SCHEMA_VERSION,
            root,
            next_id: pane_id(6),
            nodes: vec![
                PaneNodeRecord::split(
                    root,
                    None,
                    PaneSplit {
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                        first: left,
                        second: right_split,
                    },
                ),
                PaneNodeRecord::leaf(left, Some(root), PaneLeaf::new("left")),
                PaneNodeRecord::split(
                    right_split,
                    Some(root),
                    PaneSplit {
                        axis: SplitAxis::Vertical,
                        ratio: PaneSplitRatio::new(1, 1).expect("valid ratio"),
                        first: right_top,
                        second: right_bottom,
                    },
                ),
                PaneNodeRecord::leaf(right_top, Some(right_split), PaneLeaf::new("right_top")),
                PaneNodeRecord::leaf(
                    right_bottom,
                    Some(right_split),
                    PaneLeaf::new("right_bottom"),
                ),
            ],
            extensions: std::collections::BTreeMap::new(),
        };
        PaneTree::from_snapshot(snapshot).expect("valid nested pane tree")
    }

    fn root_first_share_bps(tree: &PaneTree, split: PaneId) -> u32 {
        match &tree.node(split).expect("split node present").kind {
            PaneNodeKind::Split(node) => {
                node.ratio.numerator() * 10_000
                    / (node.ratio.numerator() + node.ratio.denominator())
            }
            PaneNodeKind::Leaf(_) => panic!("expected split node"),
        }
    }

    fn apply_dispatch(
        tree: &mut PaneTree,
        layout: &PaneLayout,
        dispatch: &PanePointerDispatch,
        neutral: PanePressureSnapProfile,
        seed: &mut u64,
    ) -> usize {
        let Some(transition) = dispatch.transition.as_ref() else {
            return 0;
        };
        let pressure = dispatch.pressure_snap_profile().unwrap_or(neutral);
        let ops = tree.operations_for_transition(transition, layout, pressure);
        let applied = ops.len();
        for op in ops {
            tree.apply_operation(*seed, op).expect("operation applies");
            *seed += 1;
        }
        applied
    }

    /// Web parity for the runtime's `pane_terminal_adapter_drives_live_tree_mutation`:
    /// a pointer-capture drag flows through `PanePointerCaptureAdapter` into
    /// `PaneDragResizeTransition` values, the layout bridge
    /// (`PaneTree::operations_for_transition`) converts each geometry-bearing
    /// transition into `PaneOperation` values, and `apply_operation` moves the
    /// live root split ratio. This closes the web-side gap where the adapter
    /// emitted transitions that no in-tree host consumed.
    #[test]
    fn web_pointer_capture_drives_live_tree_mutation() {
        let mut tree = nested_pane_tree();
        let layout = tree
            .solve_layout(Rect::new(0, 0, 50, 20))
            .expect("layout should solve");
        let root_split = pane_id(1);
        let resize_target = PaneResizeTarget {
            split_id: root_split,
            axis: SplitAxis::Horizontal,
        };
        let neutral = PanePressureSnapProfile {
            strength_bps: 5_000,
            hysteresis_bps: 100,
        };

        let before = root_first_share_bps(&tree, root_split);

        let mut adapter = adapter();
        let mut op_seed = 9_000u64;
        let mut geometry_transitions = 0usize;

        // Pointer-down arms the capture machine (Armed transition carries no
        // geometry, so the bridge yields no operations yet).
        let down = adapter.pointer_down(
            resize_target,
            7,
            PanePointerButton::Primary,
            pos(25, 10),
            PaneModifierSnapshot::default(),
        );
        geometry_transitions += apply_dispatch(&mut tree, &layout, &down, neutral, &mut op_seed);

        // Drag rightward across cells, applying each transition to the tree.
        for x in [30, 36, 42] {
            let moved = adapter.pointer_move(7, pos(x, 10), PaneModifierSnapshot::default());
            geometry_transitions +=
                apply_dispatch(&mut tree, &layout, &moved, neutral, &mut op_seed);
        }

        // Release commits the gesture.
        let up = adapter.pointer_up(
            7,
            PanePointerButton::Primary,
            pos(42, 10),
            PaneModifierSnapshot::default(),
        );
        geometry_transitions += apply_dispatch(&mut tree, &layout, &up, neutral, &mut op_seed);

        assert!(
            geometry_transitions > 0,
            "drag lifecycle should yield at least one geometry-bearing transition"
        );
        let after = root_first_share_bps(&tree, root_split);
        assert!(
            after > before,
            "rightward pointer drag should grow the first child: after={after} before={before}"
        );
        assert!(matches!(adapter.machine_state(), PaneDragResizeState::Idle));
    }

    #[test]
    fn pointer_down_arms_machine_and_requests_capture() {
        let mut adapter = adapter();
        let dispatch = adapter.pointer_down(
            target(),
            11,
            PanePointerButton::Primary,
            pos(5, 8),
            PaneModifierSnapshot::default(),
        );
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Acquire { pointer_id: 11 })
        );
        assert_eq!(adapter.active_pointer_id(), Some(11));
        assert!(matches!(
            adapter.machine_state(),
            PaneDragResizeState::Armed { pointer_id: 11, .. }
        ));
        assert!(matches!(
            dispatch
                .transition
                .as_ref()
                .expect("transition should exist")
                .effect,
            PaneDragResizeEffect::Armed { pointer_id: 11, .. }
        ));
    }

    #[test]
    fn single_touch_down_uses_primary_pointer_capture_path() {
        let mut adapter = adapter();
        let dispatch =
            adapter.touch_pointer_down(target(), 14, pos(5, 8), 1, PaneModifierSnapshot::default());

        assert_eq!(dispatch.log.phase, PanePointerLifecyclePhase::PointerDown);
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Acquire { pointer_id: 14 })
        );
        assert_eq!(adapter.active_pointer_id(), Some(14));
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::PointerDown {
                pointer_id: 14,
                button: PanePointerButton::Primary,
                ..
            }
        ));
    }

    #[test]
    fn second_touch_yields_to_native_gesture_and_releases_capture() {
        let mut adapter = adapter();
        adapter.touch_pointer_down(target(), 21, pos(3, 3), 1, PaneModifierSnapshot::default());
        let ack = adapter.capture_acquired(21);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);

        let dispatch =
            adapter.touch_pointer_down(target(), 22, pos(6, 6), 2, PaneModifierSnapshot::default());

        assert_eq!(
            dispatch.log.phase,
            PanePointerLifecyclePhase::NativeTouchGesture
        );
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic cancel expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::PointerCancel,
                ..
            }
        ));
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 21 })
        );
        assert_eq!(dispatch.log.pointer_id, Some(21));
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn native_touch_gesture_without_active_capture_is_ignored() {
        let mut adapter = adapter();
        let dispatch = adapter.native_touch_gesture();

        assert_eq!(
            dispatch.log.phase,
            PanePointerLifecyclePhase::NativeTouchGesture
        );
        assert_eq!(dispatch.semantic_event, None);
        assert_eq!(
            dispatch.log.outcome,
            PanePointerLogOutcome::Ignored(PanePointerIgnoredReason::NativeTouchGesture)
        );
        assert_eq!(adapter.active_pointer_id(), None);
    }

    #[test]
    fn non_activation_button_is_ignored_deterministically() {
        let mut adapter = adapter();
        let dispatch = adapter.pointer_down(
            target(),
            3,
            PanePointerButton::Secondary,
            pos(1, 1),
            PaneModifierSnapshot::default(),
        );
        assert_eq!(dispatch.semantic_event, None);
        assert_eq!(dispatch.transition, None);
        assert_eq!(dispatch.capture_command, None);
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(
            dispatch.log.outcome,
            PanePointerLogOutcome::Ignored(PanePointerIgnoredReason::ButtonNotAllowed)
        );
    }

    #[test]
    fn pointer_move_mismatch_is_ignored_without_state_mutation() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            9,
            PanePointerButton::Primary,
            pos(10, 10),
            PaneModifierSnapshot::default(),
        );
        let before = adapter.machine_state();
        let dispatch = adapter.pointer_move(77, pos(14, 14), PaneModifierSnapshot::default());
        assert_eq!(dispatch.semantic_event, None);
        assert_eq!(
            dispatch.log.outcome,
            PanePointerLogOutcome::Ignored(PanePointerIgnoredReason::PointerMismatch)
        );
        assert_eq!(before, adapter.machine_state());
        assert_eq!(adapter.active_pointer_id(), Some(9));
    }

    #[test]
    fn pointer_up_releases_capture_and_returns_idle() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            9,
            PanePointerButton::Primary,
            pos(1, 1),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(9);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);
        let dispatch = adapter.pointer_up(
            9,
            PanePointerButton::Primary,
            pos(6, 1),
            PaneModifierSnapshot::default(),
        );
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 9 })
        );
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::PointerUp { pointer_id: 9, .. }
        ));
    }

    #[test]
    fn pointer_move_emits_motion_and_pressure_snap_profile() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            17,
            PanePointerButton::Primary,
            pos(4, 4),
            PaneModifierSnapshot::default(),
        );

        let dispatch = adapter.pointer_move(17, pos(18, 8), PaneModifierSnapshot::default());
        let expected_motion = PaneMotionVector::from_delta(14, 4, 16, 0);

        assert_eq!(dispatch.motion, Some(expected_motion));
        assert_eq!(
            dispatch.pressure_snap_profile(),
            Some(PanePressureSnapProfile::from_motion(expected_motion))
        );
    }

    #[test]
    fn pointer_move_event_delta_matches_position_step() {
        // record_pointer_step now returns the step delta that the PointerMove
        // event reuses (one subtraction, not two). The emitted delta must equal
        // the raw position step measured from the previous committed position at
        // every move — proving the reuse is exactly equivalent to the prior
        // inline computation.
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            31,
            PanePointerButton::Primary,
            pos(10, 10),
            PaneModifierSnapshot::default(),
        );
        let mut prev = pos(10, 10);
        for &(x, y) in &[(24, 12), (20, 18), (35, 18)] {
            let here = pos(x, y);
            let dispatch = adapter.pointer_move(31, here, PaneModifierSnapshot::default());
            match &dispatch
                .semantic_event
                .as_ref()
                .expect("move emits a semantic event")
                .kind
            {
                PaneSemanticInputEventKind::PointerMove {
                    delta_x,
                    delta_y,
                    position,
                    ..
                } => {
                    assert_eq!(*delta_x, here.x - prev.x);
                    assert_eq!(*delta_y, here.y - prev.y);
                    assert_eq!(position.x, here.x);
                    assert_eq!(position.y, here.y);
                }
                other => panic!("expected PointerMove, got {other:?}"),
            }
            // last_position advances only on a committed transition; mirror that.
            if dispatch.motion.is_some() {
                prev = here;
            }
        }
    }

    #[test]
    fn pointer_move_tracks_direction_changes_in_motion_summary() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            23,
            PanePointerButton::Primary,
            pos(10, 10),
            PaneModifierSnapshot::default(),
        );

        let first = adapter.pointer_move(23, pos(24, 10), PaneModifierSnapshot::default());
        let second = adapter.pointer_move(23, pos(18, 10), PaneModifierSnapshot::default());

        assert_eq!(
            first.motion,
            Some(PaneMotionVector::from_delta(14, 0, 16, 0))
        );
        assert_eq!(
            second.motion,
            Some(PaneMotionVector::from_delta(8, 0, 32, 1))
        );
        assert!(
            second
                .pressure_snap_profile()
                .expect("pressure profile should be derived from motion")
                .strength_bps
                < first
                    .pressure_snap_profile()
                    .expect("pressure profile should be derived from motion")
                    .strength_bps
        );
    }

    #[test]
    fn pointer_move_zero_delta_does_not_count_as_direction_change() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            31,
            PanePointerButton::Primary,
            pos(10, 10),
            PaneModifierSnapshot::default(),
        );

        let first = adapter.pointer_move(31, pos(24, 10), PaneModifierSnapshot::default());
        let stationary = adapter.pointer_move(31, pos(24, 10), PaneModifierSnapshot::default());
        let second = adapter.pointer_move(31, pos(18, 10), PaneModifierSnapshot::default());

        assert_eq!(
            first.motion,
            Some(PaneMotionVector::from_delta(14, 0, 16, 0))
        );
        assert_eq!(
            stationary.motion,
            Some(PaneMotionVector::from_delta(14, 0, 32, 0))
        );
        assert_eq!(
            second.motion,
            Some(PaneMotionVector::from_delta(8, 0, 48, 0))
        );
    }

    #[test]
    fn pointer_up_exposes_inertial_throw_and_projected_pointer() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            29,
            PanePointerButton::Primary,
            pos(2, 3),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(29);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);

        let drag = adapter.pointer_move(29, pos(28, 11), PaneModifierSnapshot::default());
        let release = adapter.pointer_up(
            29,
            PanePointerButton::Primary,
            pos(31, 12),
            PaneModifierSnapshot::default(),
        );
        let expected_motion = drag.motion.expect("drag motion should be recorded");
        let expected_inertial = PaneInertialThrow::from_motion(expected_motion);

        assert_eq!(release.motion, Some(expected_motion));
        assert_eq!(release.inertial_throw, Some(expected_inertial));
        assert_eq!(
            release.projected_position,
            Some(expected_inertial.projected_pointer(pos(31, 12)))
        );
    }

    #[test]
    fn pointer_up_with_wrong_button_is_ignored() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            4,
            PanePointerButton::Primary,
            pos(2, 2),
            PaneModifierSnapshot::default(),
        );
        let dispatch = adapter.pointer_up(
            4,
            PanePointerButton::Secondary,
            pos(3, 2),
            PaneModifierSnapshot::default(),
        );
        assert_eq!(
            dispatch.log.outcome,
            PanePointerLogOutcome::Ignored(PanePointerIgnoredReason::ButtonMismatch)
        );
        assert_eq!(adapter.active_pointer_id(), Some(4));
    }

    #[test]
    fn blur_emits_semantic_blur_and_releases_capture() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            6,
            PanePointerButton::Primary,
            pos(0, 0),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(6);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);
        let dispatch = adapter.blur();
        assert_eq!(dispatch.log.phase, PanePointerLifecyclePhase::Blur);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Blur { .. }
        ));
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 6 })
        );
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn blur_before_capture_ack_clears_state_without_release() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            61,
            PanePointerButton::Primary,
            pos(0, 0),
            PaneModifierSnapshot::default(),
        );
        let dispatch = adapter.blur();
        assert_eq!(dispatch.log.phase, PanePointerLifecyclePhase::Blur);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Blur { .. }
        ));
        assert_eq!(dispatch.capture_command, None);
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn visibility_hidden_emits_focus_lost_cancel() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            8,
            PanePointerButton::Primary,
            pos(5, 2),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(8);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);
        let dispatch = adapter.visibility_hidden();
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::FocusLost,
                ..
            }
        ));
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 8 })
        );
        assert_eq!(adapter.active_pointer_id(), None);
    }

    #[test]
    fn visibility_hidden_before_capture_ack_cancels_without_release() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            81,
            PanePointerButton::Primary,
            pos(5, 2),
            PaneModifierSnapshot::default(),
        );
        let dispatch = adapter.visibility_hidden();
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::FocusLost,
                ..
            }
        ));
        assert_eq!(dispatch.capture_command, None);
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn lost_pointer_capture_cancels_without_double_release() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            42,
            PanePointerButton::Primary,
            pos(7, 7),
            PaneModifierSnapshot::default(),
        );
        let dispatch = adapter.lost_pointer_capture(42);
        assert_eq!(dispatch.capture_command, None);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::PointerCancel,
                ..
            }
        ));
        assert_eq!(adapter.active_pointer_id(), None);
    }

    #[test]
    fn pointer_leave_before_capture_ack_cancels() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            31,
            PanePointerButton::Primary,
            pos(1, 1),
            PaneModifierSnapshot::default(),
        );
        let dispatch = adapter.pointer_leave(31);
        assert_eq!(dispatch.log.phase, PanePointerLifecyclePhase::PointerLeave);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::PointerCancel,
                ..
            }
        ));
        assert_eq!(dispatch.capture_command, None);
        assert_eq!(adapter.active_pointer_id(), None);
    }

    #[test]
    fn pointer_cancel_after_capture_ack_releases_and_cancels() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            39,
            PanePointerButton::Primary,
            pos(3, 3),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(39);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);

        let dispatch = adapter.pointer_cancel(Some(39));
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::PointerCancel,
                ..
            }
        ));
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 39 })
        );
        assert_eq!(adapter.active_pointer_id(), None);
    }

    #[test]
    fn pointer_cancel_without_pointer_id_releases_active_capture() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            74,
            PanePointerButton::Primary,
            pos(2, 3),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(74);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);
        let dispatch = adapter.pointer_cancel(None);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::PointerCancel,
                ..
            }
        ));
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 74 })
        );
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn pointer_leave_after_capture_ack_is_ignored() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            55,
            PanePointerButton::Primary,
            pos(4, 4),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(55);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);

        let dispatch = adapter.pointer_leave(55);
        assert_eq!(dispatch.semantic_event, None);
        assert_eq!(
            dispatch.log.outcome,
            PanePointerLogOutcome::Ignored(PanePointerIgnoredReason::LeaveWhileCaptured)
        );
        assert_eq!(adapter.active_pointer_id(), Some(55));
    }

    #[test]
    fn context_lost_releases_capture_and_cancels_with_explicit_reason() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            91,
            PanePointerButton::Primary,
            pos(5, 5),
            PaneModifierSnapshot::default(),
        );
        let ack = adapter.capture_acquired(91);
        assert_eq!(ack.log.outcome, PanePointerLogOutcome::CaptureStateUpdated);

        let dispatch = adapter.context_lost();

        assert_eq!(dispatch.log.phase, PanePointerLifecyclePhase::ContextLost);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::ContextLost,
                ..
            }
        ));
        assert_eq!(
            dispatch.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 91 })
        );
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }

    #[test]
    fn render_stalled_before_capture_ack_cancels_without_release() {
        let mut adapter = adapter();
        adapter.pointer_down(
            target(),
            92,
            PanePointerButton::Primary,
            pos(8, 6),
            PaneModifierSnapshot::default(),
        );

        let dispatch = adapter.render_stalled();

        assert_eq!(dispatch.log.phase, PanePointerLifecyclePhase::RenderStalled);
        assert!(matches!(
            dispatch
                .semantic_event
                .as_ref()
                .expect("semantic event expected")
                .kind,
            PaneSemanticInputEventKind::Cancel {
                reason: PaneCancelReason::RenderStalled,
                ..
            }
        ));
        assert_eq!(dispatch.capture_command, None);
        assert_eq!(adapter.active_pointer_id(), None);
        assert_eq!(adapter.machine_state(), PaneDragResizeState::Idle);
    }
}
