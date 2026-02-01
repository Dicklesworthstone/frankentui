#![forbid(unsafe_code)]

//! Terminal capability model.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub in_tmux: bool,
    pub in_screen: bool,
    pub in_zellij: bool,
}

impl TerminalCapabilities {
    pub fn in_any_mux(&self) -> bool {
        self.in_tmux || self.in_screen || self.in_zellij
    }
}
