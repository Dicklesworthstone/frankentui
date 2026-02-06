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

use std::fmt;

// --- Core re-exports -------------------------------------------------------

pub use ftui_core::cursor::{CursorManager, CursorSaveStrategy};
pub use ftui_core::event::{
    ClipboardEvent, ClipboardSource, Event, KeyCode, KeyEvent, KeyEventKind, Modifiers,
    MouseButton, MouseEvent, MouseEventKind, PasteEvent,
};
pub use ftui_core::terminal_capabilities::TerminalCapabilities;
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
    AdaptiveColor, Ansi16, Color, ColorCache, ColorProfile, MonoColor, ResolvedTheme, Rgb, Style,
    StyleFlags, StyleId, StyleSheet, TablePresetId, TableTheme, Theme, ThemeBuilder,
};

// --- Runtime re-exports ----------------------------------------------------

pub use ftui_runtime::{
    App, Cmd, EffectQueueConfig, InlineAutoRemeasureConfig, Locale, LocaleContext, LocaleOverride,
    Model, Program, ProgramConfig, ResizeBehavior, RuntimeDiffConfig, ScreenMode, TaskSpec,
    TerminalWriter, UiAnchor, current_locale, detect_system_locale, set_locale,
};

// --- Errors ---------------------------------------------------------------

/// Top-level error type for ftui apps.
#[derive(Debug)]
pub enum Error {
    /// I/O failure during terminal operations.
    Io(std::io::Error),
    /// Terminal or runtime error with message.
    Terminal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Terminal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Standard result type for ftui APIs.
pub type Result<T> = std::result::Result<T, Error>;

// --- Prelude --------------------------------------------------------------

pub mod prelude {
    pub use crate::{
        App, Buffer, Cmd, Error, Event, Frame, KeyCode, KeyEvent, Model, Modifiers, Result,
        ScreenMode, Style, TablePresetId, TableTheme, TerminalSession, TerminalWriter, Theme,
    };

    pub use crate::{core, layout, render, runtime, style, text, widgets};
}

pub use ftui_core as core;
pub use ftui_layout as layout;
pub use ftui_render as render;
pub use ftui_runtime as runtime;
pub use ftui_style as style;
pub use ftui_text as text;
pub use ftui_widgets as widgets;

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
        let err = Error::Terminal("something broke".into());
        assert_eq!(format!("{err}"), "something broke");
    }

    #[test]
    fn error_io_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = Error::Io(io_err);
        assert_eq!(format!("{err}"), "access denied");
    }

    #[test]
    fn error_debug() {
        let err = Error::Terminal("test".into());
        let debug = format!("{err:?}");
        assert!(debug.contains("Terminal"));
    }

    #[test]
    fn error_is_std_error() {
        let err = Error::Terminal("msg".into());
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn result_type_alias_works() {
        fn returns_ok() -> Result<i32> {
            Ok(42)
        }
        assert_eq!(returns_ok().unwrap(), 42);

        let err: Result<i32> = Err(Error::Terminal("fail".into()));
        assert!(err.is_err());
    }

    #[test]
    fn prelude_re_exports_core_types() {
        // Verify key types are accessible via prelude
        use crate::prelude::*;
        let _mode = ScreenMode::Inline { ui_height: 10 };
    }
}
