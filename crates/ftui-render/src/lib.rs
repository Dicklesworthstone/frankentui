#![forbid(unsafe_code)]

//! Render kernel: cells, buffers, diffs, and ANSI presentation.

pub mod ansi;
pub mod budget;
pub mod buffer;
pub mod cell;
pub mod counting_writer;
pub mod diff;
pub mod drawing;
pub mod frame;
pub mod grapheme_pool;
pub mod headless;
pub mod link_registry;
pub mod presenter;
pub mod sanitize;
pub mod terminal_model;
