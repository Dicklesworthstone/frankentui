#![forbid(unsafe_code)]

//! FrankenTUI public facade crate.
//!
//! This crate provides the stable, ergonomic surface area for users.

pub mod prelude {
    pub use ftui_core as core;
    pub use ftui_layout as layout;
    pub use ftui_render as render;
    pub use ftui_runtime as runtime;
    pub use ftui_style as style;
    pub use ftui_text as text;
    pub use ftui_widgets as widgets;
}
