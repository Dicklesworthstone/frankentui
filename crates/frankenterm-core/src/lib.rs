#![forbid(unsafe_code)]

//! Host-agnostic VT/ANSI terminal engine.
//!
//! `frankenterm-core` is the platform-independent terminal model at the heart of
//! FrankenTerm. It owns grid state, VT/ANSI parsing, cursor positioning, and
//! scrollback — all without any host I/O dependencies.
//!
//! # Primary responsibilities
//!
//! - **Grid**: 2D cell matrix representing the visible terminal viewport.
//! - **Cell**: character content + SGR attributes (colors, bold, italic, etc.).
//! - **Parser**: VT/ANSI state machine (Paul Flo Williams model, 12 states).
//! - **Cursor**: position, visibility, and origin/autowrap mode tracking.
//! - **Modes**: DEC private modes and ANSI standard modes.
//! - **Patch**: minimal diff between two grid snapshots for efficient updates.
//! - **Scrollback**: ring buffer for lines scrolled off the top of the viewport.
//!
//! # Design principles
//!
//! - **No I/O**: all types are pure data + logic; the host adapter supplies bytes.
//! - **Deterministic**: identical byte sequences always produce identical state.
//! - **`#![forbid(unsafe_code)]`**: safety enforced at compile time.

pub mod cell;
pub mod cursor;
pub mod flow_control;
pub mod grid;
pub mod modes;
pub mod parser;
pub mod patch;
pub mod reply;
pub mod scrollback;
pub mod selection;

pub use cell::{Cell, CellFlags, Color, HyperlinkId, HyperlinkRegistry, SgrAttrs, SgrFlags};
pub use cursor::{Cursor, SavedCursor, translate_charset};
pub use flow_control::{
    ActionLoss, BackpressureAction, DecisionReason, FlowControlConfig, FlowControlDecision,
    FlowControlPolicy, FlowControlSnapshot, InputEventClass, LatencyWindowMs, LossWeights,
    QueueDepthBytes, RateWindowBps, jain_fairness_index,
};
pub use grid::Grid;
pub use modes::{AnsiModes, DecModes, Modes};
pub use parser::{Action, CsiParams, Parser};
pub use patch::{CellUpdate, ChangeRun, DirtySpan, DirtyTracker, GridDiff, Patch};
pub use reply::{
    ReplyContext, ReplyEngine, TerminalQuery, parse_terminal_query, reply_for_query,
    reply_for_query_bytes,
};
pub use scrollback::{Scrollback, ScrollbackLine, ScrollbackWindow};
pub use selection::{BufferPos, Selection};
