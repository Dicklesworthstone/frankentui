#![forbid(unsafe_code)]

//! WASM frontend for FrankenTerm.
//!
//! This crate is intentionally host-specific (web/WASM). The initial goal is to
//! provide a stable `wasm-bindgen` API surface for:
//! - feeding bytes (remote VT/ANSI streams),
//! - applying cell patches (client-side ftui mode),
//! - capturing web input events,
//! - driving rendering.
//!
//! The actual WebGPU renderer and full input system will be implemented behind
//! this API.

pub mod glyph_atlas;
pub mod input;
pub mod renderer;

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::FrankenTermWeb;

/// Native builds compile this crate as a stub so `cargo check --workspace` stays
/// green on non-wasm targets.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct FrankenTermWeb;

#[cfg(not(target_arch = "wasm32"))]
impl FrankenTermWeb {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self
    }
}
