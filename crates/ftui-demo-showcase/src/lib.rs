#![forbid(unsafe_code)]
#![allow(dead_code)]
#![recursion_limit = "256"]

//! FrankenTUI Demo Showcase library.
//!
//! This module exposes the demo application internals so that integration tests
//! can construct screens, render them, and assert snapshots.
//!
//! # Role in FrankenTUI
//! `ftui-demo-showcase` is the primary way to see what the system can do.
//! It aggregates the visual effects, widgets, layouts, and interaction demos
//! into a single runnable application.
//!
//! # How it fits in the system
//! The demo showcase is a consumer of all core crates. It exercises the runtime,
//! render kernel, widgets, and extras in one place, and its screens are used
//! as the reference for snapshots and visual regression tests.

pub mod app;
pub mod assets;
pub mod chrome;
pub mod cli;
pub mod data;
pub mod determinism;
pub mod screens;
pub mod test_logging;
pub mod theme;
pub mod tour;

/// Debug logging macro for visual render diagnostics (bd-3vbf.31).
///
/// Only emits to stderr when `debug-render` feature is enabled.
/// Usage: `debug_render!("dashboard", "layout={layout:?}, area={area:?}");`
#[cfg(feature = "debug-render")]
#[macro_export]
macro_rules! debug_render {
    ($component:expr, $($arg:tt)*) => {
        eprintln!("[debug-render][{}] {}", $component, format_args!($($arg)*));
    };
}

#[cfg(not(feature = "debug-render"))]
#[macro_export]
macro_rules! debug_render {
    ($component:expr, $($arg:tt)*) => {};
}
