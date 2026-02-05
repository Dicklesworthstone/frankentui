//! Optional feature-gated extensions for FrankenTUI.
//!
//! Each module is behind a Cargo feature flag and can be enabled independently.
//! These modules provide higher-level functionality built on top of the core
//! ftui crates (render, style, text, widgets).
//!
//! # Role in FrankenTUI
//! `ftui-extras` collects optional, higher-level utilities that are useful
//! across apps but not required by the core runtime or render kernel. It is
//! feature-gated to keep the base dependency graph lean.
//!
//! # How it fits in the system
//! Extras build on `ftui-render`, `ftui-style`, `ftui-text`, and `ftui-widgets`.
//! You can enable only the pieces you need without pulling in everything else.
//!
//! # Available Features
//!
//! | Feature | Module | Description |
//! |---------|--------|-------------|
//! | `canvas` | [`canvas`] | Pixel-level drawing primitives |
//! | `charts` | [`charts`] | Chart widgets (depends on canvas) |
//! | `clipboard` | [`clipboard`] | OSC 52 clipboard integration |
//! | `diagram` | [`diagram`] | ASCII diagram detection and correction |
//! | `console` | [`console`] | ANSI-aware console text processing |
//! | `export` | [`export`] | Buffer export to HTML/SVG/text |
//! | `filesize` | [`filesize`] | Human-readable file size formatting |
//! | `filepicker` | [`filepicker`] | File picker state utilities |
//! | `forms` | [`forms`] | Form layout and input widgets |
//! | `validation` | [`validation`] | Form validation framework with composable validators |
//! | `image` | [`image`] | Terminal image protocols (iTerm2/Kitty) |
//! | `live` | [`live`] | Live-updating display (depends on console) |
//! | `logging` | [`logging`] | Tracing subscriber for TUI logging |
//! | `markdown` | [`markdown`] | Markdown to styled text rendering |
//! | `pty-capture` | [`pty_capture`] | PTY session capture |
//! | `stopwatch` | [`stopwatch`] | Stopwatch timing utility |
//! | `syntax` | [`syntax`] | Syntax highlighting spans |
//! | `timer` | [`timer`] | Countdown timer utility |
//! | `traceback` | [`traceback`] | Error/stacktrace display |
//! | `theme` | [`theme`] | Color themes + palette tokens |
//! | `terminal` | [`terminal`] | ANSI escape sequence parser for terminal emulation |
//! | `text-effects` | [`text_effects`] | Animated text effects (gradients, fades, ASCII art) |
//! | `visual-fx` | [`visual_fx`] | Feature-gated visual FX primitives (backdrops, CPU/GPU adapters) |
//! | `fx-gpu` | `visual_fx::gpu` | Optional GPU acceleration for metaballs (silent CPU fallback) |
//! | `help` | [`help`] | Contextual help system with tooltips |

#![forbid(unsafe_code)]

#[cfg(test)]
use stats_alloc::{INSTRUMENTED_SYSTEM, StatsAlloc};
#[cfg(test)]
use std::alloc::System;

#[cfg(test)]
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[cfg(feature = "canvas")]
pub mod canvas;

#[cfg(feature = "console")]
pub mod console;

#[cfg(feature = "charts")]
pub mod charts;

#[cfg(feature = "clipboard")]
pub mod clipboard;

#[cfg(feature = "diagram")]
pub mod diagram;

#[cfg(feature = "diagram")]
pub mod mermaid;

#[cfg(feature = "diagram")]
pub mod mermaid_layout;

#[cfg(feature = "diagram")]
pub mod diagram_layout;

#[cfg(feature = "diagram")]
pub mod mermaid_render;

#[cfg(feature = "export")]
pub mod export;

#[cfg(feature = "filesize")]
pub mod filesize;

#[cfg(feature = "forms")]
pub mod forms;

#[cfg(feature = "validation")]
pub mod validation;

#[cfg(feature = "image")]
pub mod image;

#[cfg(feature = "live")]
pub mod live;

#[cfg(feature = "logging")]
pub mod logging;

#[cfg(feature = "markdown")]
pub mod markdown;

#[cfg(feature = "pty-capture")]
pub mod pty_capture;

#[cfg(feature = "syntax")]
pub mod syntax;

#[cfg(feature = "filepicker")]
pub mod filepicker;

#[cfg(feature = "traceback")]
pub mod traceback;

#[cfg(feature = "stopwatch")]
pub mod stopwatch;

#[cfg(feature = "timer")]
pub mod timer;

#[cfg(feature = "theme")]
pub mod theme;

#[cfg(feature = "terminal")]
pub mod terminal;

#[cfg(feature = "text-effects")]
pub mod text_effects;

#[cfg(feature = "visual-fx")]
pub mod visual_fx;

#[cfg(feature = "help")]
pub mod help;

#[cfg(feature = "doom")]
pub mod doom;
