#![forbid(unsafe_code)]

//! FrankenTUI public facade crate.
//!
//! # Role in FrankenTUI
//! This crate is the user-facing entry point for the ecosystem. It re-exports
//! the most commonly used types from the internal crates (core/render/layout/
//! runtime/widgets/style/text/extras) so application code does not need to wire
//! each crate individually.
//!
//! # What belongs here
//! - Stable public surface area (re-exports).
//! - Minimal glue and convenience APIs.
//! - A lightweight prelude for day-to-day use.
//!
//! # How it fits in the system
//! - Input layer: provided by `ftui-core`
//! - Runtime loop: provided by `ftui-runtime`
//! - Render kernel: provided by `ftui-render`
//! - Layout, text, style, and widgets: provided by their respective crates
//! - This crate ties them together for application authors.
//!
//! If you only depend on one crate in your application, it should be `ftui`.

pub mod error;

// --- Core re-exports -------------------------------------------------------

pub use ftui_core::cursor::{CursorManager, CursorSaveStrategy};
pub use ftui_core::cx::{Cx, CxController, CxError, LabClock};
pub use ftui_core::event::{
    ClipboardEvent, ClipboardSource, Event, KeyCode, KeyEvent, KeyEventKind, Modifiers,
    MouseButton, MouseEvent, MouseEventKind, PasteEvent,
};
pub use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
#[cfg(all(not(target_arch = "wasm32"), feature = "crossterm"))]
pub use ftui_core::terminal_session::{SessionOptions, TerminalSession};

// --- Render re-exports -----------------------------------------------------

pub use ftui_render::buffer::Buffer;
pub use ftui_render::cell::{Cell, CellAttrs, PackedRgba};
pub use ftui_render::diff::BufferDiff;
pub use ftui_render::frame::Frame;
pub use ftui_render::grapheme_pool::GraphemePool;
pub use ftui_render::link_registry::LinkRegistry;
pub use ftui_render::presenter::Presenter;

// --- Style re-exports ------------------------------------------------------

pub use ftui_style::{
    AdaptiveColor, Ansi16, Color, ColorCache, MonoColor, ResolvedTheme, Rgb, Style, StyleFlags,
    StyleId, StyleSheet, TablePresetId, TableTheme, Theme, ThemeBuilder,
};

// --- Runtime re-exports (feature-gated for wasm32 compatibility) ----------

#[cfg(feature = "runtime")]
pub use ftui_runtime::{
    App, Cmd, EffectQueueConfig, InlineAutoRemeasureConfig, Locale, LocaleContext, LocaleOverride,
    Model, PredictiveConfig, Program, ProgramConfig, ResizeBehavior, RuntimeDiffConfig, ScreenMode,
    TaskSpec, TerminalWriter, TickDecision, TickStrategy, TickStrategyKind, UiAnchor,
    current_locale, detect_system_locale, set_locale,
};

// --- Errors ---------------------------------------------------------------

pub use error::{
    DegradationAction, Error, LayoutError, ProtocolError, RenderError, Result, TerminalError,
    WidgetError,
};

// --- Prelude --------------------------------------------------------------

pub mod prelude {
    #[cfg(all(not(target_arch = "wasm32"), feature = "crossterm"))]
    pub use crate::TerminalSession;
    pub use crate::{
        Buffer, Cx, CxController, CxError, Error, Event, Frame, KeyCode, KeyEvent, LabClock,
        Modifiers, Result, Style, TablePresetId, TableTheme, Theme,
    };

    #[cfg(feature = "runtime")]
    pub use crate::{
        App, Cmd, Model, PredictiveConfig, ScreenMode, TerminalWriter, TickDecision, TickStrategy,
        TickStrategyKind,
    };

    pub use crate::{core, layout, pane, render, style, text, widgets};

    #[cfg(feature = "runtime")]
    pub use crate::runtime;
}

pub use ftui_core as core;
pub use ftui_layout as layout;
pub use ftui_render as render;
#[cfg(feature = "runtime")]
pub use ftui_runtime as runtime;
pub use ftui_style as style;
pub use ftui_text as text;
pub use ftui_widgets as widgets;

// --- Pane workspace surface (stable) --------------------------------------

/// Curated, single-import entry point for the FrankenTUI pane workspace
/// system (split-tree, drag/resize, docking, keyboard, persistence).
///
/// This module is the **stable public surface** for panes. Everything
/// re-exported directly under `ftui::pane` is covered by the stability
/// contract in `docs/api/pane-stability-contract.md`: the canonical pane
/// model, semantic interaction contract, structural operations, snap/dock
/// affordances, accessibility/command vocabulary, and versioned persistence.
///
/// # Tiers
/// - **Stable** (`ftui::pane::*`): the day-to-day API. Breaking changes follow
///   the deprecation policy in the stability contract and bump the relevant
///   schema-version constant (`PANE_TREE_SCHEMA_VERSION`,
///   `PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION`).
/// - **Keyboard host adapter** (`ftui::pane::keyboard`, requires the `runtime`
///   feature): the terminal keyboard controller, focus ring, and key hints.
/// - **Advanced** (`ftui::pane::advanced`): performance/execution tuning
///   internals (memory strategy, retention, execution policy, assumption
///   monitors). These are **not** part of the stability guarantee and may
///   change between minor releases as the perf substrate evolves.
///
/// The full, uncurated layout surface remains reachable via
/// [`crate::layout`] for advanced or experimental use.
pub mod pane {
    pub use ftui_layout::pane::*;
    pub use ftui_layout::pane_command::*;
    pub use ftui_layout::pane_persistent::*;

    /// Performance/execution tuning internals for panes.
    ///
    /// **Not** part of the stable surface: these types describe the adaptive
    /// memory/retention/execution substrate and its assumption monitors, and
    /// may change between minor releases. Applications should treat the
    /// defaults as a black box unless they are explicitly tuning perf.
    pub mod advanced {
        pub use ftui_layout::pane_execution::*;
        pub use ftui_layout::pane_memory::*;
        pub use ftui_layout::pane_monitors::*;
        pub use ftui_layout::pane_retention::*;
    }

    /// Terminal keyboard host adapter for panes (requires the `runtime`
    /// feature): the keyboard controller, focus-ring rendering, key hints,
    /// and the `KeyEvent -> PaneCommand` mapping.
    #[cfg(feature = "runtime")]
    pub mod keyboard {
        pub use ftui_runtime::pane_keymap::*;
    }

    /// The most common pane types for day-to-day use.
    ///
    /// `use ftui::pane::prelude::*;` brings the split-tree, structural
    /// operations, the semantic interaction event/machine, and the keyboard
    /// command vocabulary into scope.
    pub mod prelude {
        pub use ftui_layout::pane::{
            PaneConstraints, PaneDragResizeMachine, PaneId, PaneLayout, PaneOperation,
            PaneOperationOutcome, PaneSemanticInputEvent, PaneTransaction, PaneTree, SplitAxis,
        };
        pub use ftui_layout::pane_command::PaneCommand;
        pub use ftui_layout::pane_persistent::VersionedPaneTree;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: Error = Error::from(io_err);
        match &err {
            Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            _ => panic!("expected Io variant"),
        }
    }

    #[test]
    fn error_terminal_display() {
        let err: Error = TerminalError::SessionSetup("something broke".into()).into();
        assert!(format!("{err}").contains("something broke"));
    }

    #[test]
    fn error_io_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = Error::Io(io_err);
        assert!(format!("{err}").contains("access denied"));
    }

    #[test]
    fn error_debug() {
        let err: Error = TerminalError::MissingCapability("test").into();
        let debug = format!("{err:?}");
        assert!(debug.contains("Terminal"));
    }

    #[test]
    fn error_is_std_error() {
        let err: Error = TerminalError::MissingCapability("msg").into();
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn result_type_alias_works() {
        fn returns_ok() -> Result<i32> {
            Ok(42)
        }
        assert_eq!(returns_ok().unwrap(), 42);

        let err: Result<i32> = Err(TerminalError::SessionSetup("fail".into()).into());
        assert!(err.is_err());
    }

    #[test]
    #[cfg(feature = "runtime")]
    fn prelude_re_exports_core_types() {
        // Verify key types are accessible via prelude
        use crate::prelude::*;
        let _mode = ScreenMode::Inline { ui_height: 10 };
    }

    #[test]
    fn pane_surface_is_reachable_via_facade() {
        // Stable surface: the canonical model + schema versions resolve through
        // `ftui::pane` without reaching into `ftui::layout` internals.
        use crate::pane::{PANE_TREE_SCHEMA_VERSION, PaneTree};
        let tree = PaneTree::singleton("root");
        assert!(tree.validate().is_ok());
        // The serialization-contract constant is reachable via the facade.
        let _schema: u16 = PANE_TREE_SCHEMA_VERSION;
    }

    #[test]
    fn pane_prelude_brings_common_types_into_scope() {
        use crate::pane::prelude::*;
        // Construction + the keyboard command vocabulary are both in scope.
        let _tree = PaneTree::singleton("root");
        let _axis = SplitAxis::Horizontal;
        let _versioned = VersionedPaneTree::singleton("root");
        // PaneCommand is part of the prelude; ensure the type name resolves.
        fn _takes_command(_c: PaneCommand) {}
    }

    #[test]
    fn pane_advanced_tier_is_segregated() {
        // Advanced perf knobs live under `pane::advanced`, not the stable root.
        use crate::pane::advanced::PaneExecutionPolicy;
        let _ = std::mem::size_of::<PaneExecutionPolicy>();
    }
}
