#![forbid(unsafe_code)]

//! Render kernel: cells, buffers, diffs, and ANSI presentation.

pub mod buffer;
pub mod cell;
pub mod diff;
pub mod frame;
pub mod grapheme_pool;
pub mod link_registry;
pub mod presenter;
