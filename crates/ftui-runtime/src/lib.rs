#![forbid(unsafe_code)]

//! FrankenTUI Runtime
//!
//! This crate provides the runtime components that tie together the core,
//! render, and layout crates into a complete terminal application framework.
//!
//! # Key Components
//!
//! - [`TerminalWriter`] - Unified terminal output coordinator with inline mode support
//! - [`LogSink`] - Line-buffered writer for sanitized log output
//! - [`Program`] - Bubbletea/Elm-style runtime for terminal applications
//! - [`Model`] - Trait for application state and behavior
//! - [`Cmd`] - Commands for side effects
//! - [`Subscription`] - Trait for continuous event sources
//! - [`Every`] - Built-in tick subscription

pub mod input_macro;
pub mod log_sink;
pub mod program;
#[cfg(feature = "render-thread")]
pub mod render_thread;
pub mod simulator;
#[cfg(feature = "stdio-capture")]
pub mod stdio_capture;
pub mod string_model;
pub mod subscription;
pub mod terminal_writer;

pub use input_macro::{
    EventRecorder, FilteredEventRecorder, InputMacro, MacroPlayer, MacroRecorder, RecordingFilter,
    RecordingState, TimedEvent,
};
pub use log_sink::LogSink;
pub use program::{App, AppBuilder, Cmd, Model, Program, ProgramConfig};
pub use simulator::ProgramSimulator;
pub use string_model::{StringModel, StringModelAdapter};
pub use subscription::{Every, MockSubscription, StopSignal, SubId, Subscription};
pub use terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};

#[cfg(feature = "render-thread")]
pub use render_thread::{OutMsg, RenderThread};

#[cfg(feature = "stdio-capture")]
pub use stdio_capture::{CapturedWriter, StdioCapture, StdioCaptureError};
