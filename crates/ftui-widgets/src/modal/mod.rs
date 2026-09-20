#![forbid(unsafe_code)]

//! Modal container widget (overlay layer), dialog presets, modal stack management, and animations.
//!
//! # Animation System (bd-39vx.4)
//!
//! Modals support smooth entrance and exit animations:
//!
//! - **Scale-in/out**: Classic modal pop effect
//! - **Fade-in/out**: Opacity transition
//! - **Slide animations**: Slide from top/bottom
//! - **Backdrop fade**: Independent backdrop opacity animation
//! - **Reduced motion**: Respects accessibility preferences
//!
//! Use [`ModalAnimationState`] to track animation progress and compute
//! interpolated values for scale, opacity, and position.
//!
//! # Focus Management (bd-39vx.5)
//!
//! Modals can integrate with [`crate::focus::FocusManager`] for accessibility:
//!
//! - **Auto-focus**: First focusable element receives focus when modal opens
//! - **Focus trap**: Tab navigation is constrained within the modal
//! - **Focus restore**: Previous focus is restored when modal closes
//! - **Escape to close**: Already built into modal handling
//!
//! Use [`FocusAwareModalStack`] as the canonical focus-aware modal API. For
//! lower-level orchestration, pair [`ModalStack::push_with_focus`] with your own
//! `FocusManager`.
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::modal::{FocusAwareModalStack, WidgetModalEntry, ModalAnimationState};
//!
//! let mut modals = FocusAwareModalStack::new();
//! let mut animation = ModalAnimationState::new();
//!
//! // Start opening animation
//! animation.start_opening();
//!
//! // Push modal with focus trap
//! modals.push_with_trap(
//!     Box::new(WidgetModalEntry::new(dialog).with_focusable_ids(vec![1, 2, 3])),
//!     vec![1, 2, 3],
//! );
//! ```

mod animation;
mod container;
mod dialog;
pub mod focus_integration;
mod stack;

pub use animation::{
    ModalAnimationConfig, ModalAnimationPhase, ModalAnimationState, ModalEasing,
    ModalEntranceAnimation, ModalExitAnimation,
};
pub use container::{
    BackdropConfig, MODAL_HIT_BACKDROP, MODAL_HIT_CONTENT, Modal, ModalAction, ModalConfig,
    ModalPosition, ModalSizeConstraints, ModalState,
};
pub use dialog::{
    DIALOG_HIT_BUTTON, DIALOG_HIT_INPUT, Dialog, DialogBuilder, DialogButton, DialogConfig,
    DialogKind, DialogResult, DialogState,
};
pub use focus_integration::FocusAwareModalStack;
pub use stack::{
    ModalFocusId, ModalId, ModalResult, ModalResultData, ModalStack, StackModal, WidgetModalEntry,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// `HitRegion::Custom(n)` is one namespace per `HitId`, and a `Dialog`
    /// renders inside a `Modal` under the same id - the container registers
    /// the backdrop over the whole screen, then the dialog overlays its own
    /// regions. Two constants sharing a discriminant are therefore not a
    /// naming nuisance but the same region to `hit_test`, which is how
    /// `DIALOG_HIT_INPUT` and `MODAL_HIT_BACKDROP` both being `Custom(1)`
    /// turned every backdrop click into a click on the prompt's text field.
    ///
    /// These constants live in three files, so nothing but this test stops
    /// the next one from reusing a number.
    #[test]
    fn hit_regions_do_not_collide_across_the_modal_module() {
        let allocated = [
            ("MODAL_HIT_BACKDROP", MODAL_HIT_BACKDROP),
            ("MODAL_HIT_CONTENT", MODAL_HIT_CONTENT),
            ("DIALOG_HIT_INPUT", DIALOG_HIT_INPUT),
            ("DIALOG_HIT_BUTTON", DIALOG_HIT_BUTTON),
        ];
        for (i, (left_name, left)) in allocated.iter().enumerate() {
            for (right_name, right) in &allocated[i + 1..] {
                assert_ne!(
                    left, right,
                    "{left_name} and {right_name} are the same hit region, so \
                     `hit_test` cannot tell them apart under one HitId"
                );
            }
        }
    }
}
