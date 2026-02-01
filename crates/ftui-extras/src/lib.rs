#![forbid(unsafe_code)]

#[cfg(feature = "canvas")]
pub mod canvas;

#[cfg(feature = "console")]
pub mod console;

#[cfg(feature = "charts")]
pub mod charts;

#[cfg(feature = "clipboard")]
pub mod clipboard;

#[cfg(feature = "export")]
pub mod export;

#[cfg(feature = "forms")]
pub mod forms;

#[cfg(feature = "image")]
pub mod image;

#[cfg(feature = "markdown")]
pub mod markdown;

#[cfg(feature = "pty-capture")]
pub mod pty_capture;

#[cfg(feature = "syntax")]
pub mod syntax;
