#![forbid(unsafe_code)]

//! Canonical input/event types.
//!
//! NOTE: This module is intentionally minimal at first; the full contract is
//! defined by beads bd-10i.2.4 and bd-10i.5.1.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Placeholder.
    Tick,
}
