#![forbid(unsafe_code)]

//! Core: terminal lifecycle, capability detection, events, and input parsing.

pub mod event;
pub mod input_parser;
pub mod terminal_capabilities;
pub mod terminal_session;
