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
    DIALOG_HIT_BUTTON, DIALOG_HIT_INPUT, Dialog, DialogBuilder, DialogButton, DialogConfig, DialogKind,
    DialogResult, DialogState,
};
pub use focus_integration::FocusAwareModalStack;
pub use stack::{
    ModalFocusId, ModalId, ModalResult, ModalResultData, ModalStack, StackModal, WidgetModalEntry,
};
