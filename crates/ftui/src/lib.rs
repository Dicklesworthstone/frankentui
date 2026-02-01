#![forbid(unsafe_code)]

//! FrankenTUI public facade crate.
//!
//! This crate provides the stable, ergonomic surface area for users. It
//! re-exports common types from internal crates and offers a lightweight
//! prelude for day-to-day usage.

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
    StyleFlags, StyleId, StyleSheet, Theme, ThemeBuilder,
};

// --- Runtime re-exports ----------------------------------------------------

pub use ftui_runtime::{
    App, Cmd, Model, Program, ProgramConfig, ScreenMode, TerminalWriter, UiAnchor,
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
        ScreenMode, Style, TerminalSession, TerminalWriter, Theme,
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
