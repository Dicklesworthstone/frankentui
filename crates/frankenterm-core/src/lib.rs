#![forbid(unsafe_code)]

//! Terminal core logic (grid, scrollback, VT parser).

use ftui_render::cell::Cell;

#[cfg(feature = "ws-codec")]
pub mod flow_control;

/// Terminal grid.
pub struct TerminalGrid {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
}

/// VT Parser.
pub struct VtParser {}

/// Terminal Patch.
pub struct TerminalPatch {}
