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
    PaneDragResizeTransition, PaneModifierSnapshot, PanePointerButton, PanePointerPosition,
    PaneResizeTarget, PaneSemanticInputEvent, PaneSemanticInputEventKind,
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
    capture_state: CaptureState,
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
    Blur,
    VisibilityHidden,
    LostPointerCapture,
    CaptureAcquired,
}

/// Deterministic reason why an incoming lifecycle signal was ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePointerIgnoredReason {
    InvalidPointerId,
    ButtonNotAllowed,
    ButtonMismatch,
    ActivePointerAlreadyInProgress,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanePointerDispatch {
    pub semantic_event: Option<PaneSemanticInputEvent>,
    pub transition: Option<PaneDragResizeTransition>,
    pub capture_command: Option<PanePointerCaptureCommand>,
    pub log: PanePointerLogEntry,
}

impl PanePointerDispatch {
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
            self.active = Some(ActivePointerCapture {
                pointer_id,
                target,
                button,
                last_position: position,
                capture_state: CaptureState::Requested,
            });
        }
        dispatch
    }

    /// Mark browser pointer capture as successfully acquired.
    pub fn capture_acquired(&mut self, pointer_id: u32) -> PanePointerDispatch {
        let Some(mut active) = self.active else {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::CaptureAcquired,
                PanePointerIgnoredReason::NoActivePointer,
                Some(pointer_id),
                None,
                None,
            );
        };
        if active.pointer_id != pointer_id {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::CaptureAcquired,
                PanePointerIgnoredReason::PointerMismatch,
                Some(pointer_id),
                Some(active.target),
                None,
            );
        }
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
        let Some(mut active) = self.active else {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerMove,
                PanePointerIgnoredReason::NoActivePointer,
                Some(pointer_id),
                None,
                Some(position),
            );
        };
        if active.pointer_id != pointer_id {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerMove,
                PanePointerIgnoredReason::PointerMismatch,
                Some(pointer_id),
                Some(active.target),
                Some(position),
            );
        }

        let kind = PaneSemanticInputEventKind::PointerMove {
            target: active.target,
            pointer_id,
            position,
            delta_x: position.x.saturating_sub(active.last_position.x),
            delta_y: position.y.saturating_sub(active.last_position.y),
        };
        let dispatch = self.forward_semantic(
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
        let Some(active) = self.active else {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerUp,
                PanePointerIgnoredReason::NoActivePointer,
                Some(pointer_id),
                None,
                Some(position),
            );
        };
        if active.pointer_id != pointer_id {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerUp,
                PanePointerIgnoredReason::PointerMismatch,
                Some(pointer_id),
                Some(active.target),
                Some(position),
            );
        }
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
        let dispatch = self.forward_semantic(
            DispatchContext {
                phase: PanePointerLifecyclePhase::PointerUp,
                pointer_id: Some(pointer_id),
                target: Some(active.target),
                position: Some(position),
            },
            kind,
            modifiers,
            active
                .capture_state
                .is_acquired()
                .then_some(PanePointerCaptureCommand::Release { pointer_id }),
        );
        if dispatch.transition.is_some() {
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
        let Some(active) = self.active else {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerLeave,
                PanePointerIgnoredReason::NoActivePointer,
                Some(pointer_id),
                None,
                None,
            );
        };
        if active.pointer_id != pointer_id {
            return PanePointerDispatch::ignored(
                PanePointerLifecyclePhase::PointerLeave,
                PanePointerIgnoredReason::PointerMismatch,
                Some(pointer_id),
                Some(active.target),
                None,
            );
        }

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
            active
                .capture_state
                .is_acquired()
                .then_some(PanePointerCaptureCommand::Release {
                    pointer_id: active.pointer_id,
                }),
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
        let command = (release_capture && active.capture_state.is_acquired()).then_some(
            PanePointerCaptureCommand::Release {
                pointer_id: active.pointer_id,
            },
        );
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
        PanePointerIgnoredReason, PanePointerLifecyclePhase, PanePointerLogOutcome,
    };
    use ftui_layout::{
        PaneCancelReason, PaneDragResizeEffect, PaneDragResizeState, PaneId, PaneModifierSnapshot,
        PanePointerButton, PanePointerPosition, PaneResizeTarget, PaneSemanticInputEventKind,
        SplitAxis,
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
    fn pointer_leave_after_capture_ack_releases_and_cancels() {
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
}
