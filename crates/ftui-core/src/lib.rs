#![forbid(unsafe_code)]

//! Core: terminal lifecycle, capability detection, events, and input parsing.

pub mod animation;
pub mod cursor;
pub mod event;
pub mod event_coalescer;
pub mod geometry;
pub mod inline_mode;
pub mod input_parser;
pub mod logging;
pub mod mux_passthrough;
pub mod terminal_capabilities;
pub mod terminal_session;

#[cfg(feature = "caps-probe")]
pub mod caps_probe;

// Re-export tracing macros at crate root for ergonomic use.
#[cfg(feature = "tracing")]
pub use logging::{
    debug, debug_span, error, error_span, info, info_span, trace, trace_span, warn, warn_span,
};
