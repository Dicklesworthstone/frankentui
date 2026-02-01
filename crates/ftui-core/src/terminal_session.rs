#![forbid(unsafe_code)]

//! Terminal session lifecycle guard.
//!
//! This will own raw-mode entry/exit and ensure cleanup on drop.

#[derive(Debug)]
pub struct TerminalSession;

impl TerminalSession {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TerminalSession {
    fn default() -> Self {
        Self::new()
    }
}
