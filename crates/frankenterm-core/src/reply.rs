//! Terminal query/reply engine for VT/ANSI request sequences.
//!
//! The parser currently exposes unsupported CSI requests as `Action::Escape`.
//! This module decodes common terminal queries from those escape payloads and
//! emits deterministic response bytes.
//!
//! Supported requests:
//! - DSR status report: `CSI 5 n` -> `CSI 0 n`
//! - DSR cursor position report: `CSI 6 n` -> `CSI {row};{col} R` (1-indexed)
//! - DECXCPR report: `CSI ? 6 n` -> `CSI ? {row};{col} R`
//! - DA1 primary attributes: `CSI c` / `CSI 0 c` -> `CSI ?64;1;2;4;6;9;15;18;21;22 c`
//! - DA2 secondary attributes: `CSI > c` / `CSI >0 c` -> `CSI >1;10;0 c`
//! - DECRPM mode query: `CSI ? Ps $ p` -> `CSI ? Ps ; {status} $ y`

use crate::{Action, Cursor, DecModes, Modes};

const DA1_REPLY: &[u8] = b"\x1b[?64;1;2;4;6;9;15;18;21;22c";

/// Decoded terminal query extracted from an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalQuery {
    /// DSR operating-status query (`CSI 5 n`).
    DeviceStatus,
    /// DSR cursor-position query (`CSI 6 n`).
    CursorPosition,
    /// DECXCPR cursor-position query (`CSI ? 6 n`).
    ExtendedCursorPosition,
    /// DA1 primary device attributes (`CSI c` / `CSI 0 c`).
    PrimaryDeviceAttributes,
    /// DA2 secondary device attributes (`CSI > c` / `CSI >0 c`).
    SecondaryDeviceAttributes,
    /// DECRPM mode status query (`CSI ? Ps $ p`).
    DecModeReport { mode: u16 },
}

impl TerminalQuery {
    /// Attempt to decode a query from a raw escape payload.
    ///
    /// The sequence must be complete and start with `ESC [`.
    #[must_use]
    pub fn parse_escape(seq: &[u8]) -> Option<Self> {
        if seq.len() < 3 || seq[0] != 0x1b || seq[1] != b'[' {
            return None;
        }

        let (final_byte, params) = seq[2..].split_last()?;
        match *final_byte {
            b'n' => parse_dsr_query(params),
            b'c' => parse_da_query(params),
            b'p' => parse_decrpm_query(params),
            _ => None,
        }
    }
}

/// Context needed to construct terminal replies.
#[derive(Debug, Clone, Copy)]
pub struct ReplyContext<'a> {
    /// Cursor row in zero-based coordinates.
    pub cursor_row: u16,
    /// Cursor column in zero-based coordinates.
    pub cursor_col: u16,
    /// Current mode state for DECRPM answers.
    pub modes: Option<&'a Modes>,
}

/// Deterministic terminal reply encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyEngine {
    /// DA2 terminal type identifier.
    pub da2_terminal_id: u16,
    /// DA2 firmware/version field.
    pub da2_version: u16,
    /// DA2 ROM cartridge field (usually 0).
    pub da2_rom: u16,
}

impl Default for ReplyEngine {
    fn default() -> Self {
        Self::xterm_like()
    }
}

impl ReplyEngine {
    /// Build an xterm-like DA2 identity.
    #[must_use]
    pub const fn xterm_like() -> Self {
        Self {
            da2_terminal_id: 1,
            da2_version: 10,
            da2_rom: 0,
        }
    }

    /// Extract a supported query from a parser action.
    #[must_use]
    pub fn query_from_action(action: &Action) -> Option<TerminalQuery> {
        match action {
            Action::DeviceAttributes => Some(TerminalQuery::PrimaryDeviceAttributes),
            Action::DeviceAttributesSecondary => Some(TerminalQuery::SecondaryDeviceAttributes),
            Action::DeviceStatusReport => Some(TerminalQuery::DeviceStatus),
            Action::CursorPositionReport => Some(TerminalQuery::CursorPosition),
            Action::Escape(seq) => TerminalQuery::parse_escape(seq),
            _ => None,
        }
    }

    /// Encode the byte reply for a decoded query.
    #[must_use]
    pub fn reply_for_query(self, query: TerminalQuery, context: ReplyContext<'_>) -> Vec<u8> {
        match query {
            TerminalQuery::DeviceStatus => b"\x1b[0n".to_vec(),
            TerminalQuery::CursorPosition => format!(
                "\x1b[{};{}R",
                context.cursor_row.saturating_add(1),
                context.cursor_col.saturating_add(1)
            )
            .into_bytes(),
            TerminalQuery::ExtendedCursorPosition => format!(
                "\x1b[?{};{}R",
                context.cursor_row.saturating_add(1),
                context.cursor_col.saturating_add(1)
            )
            .into_bytes(),
            TerminalQuery::PrimaryDeviceAttributes => DA1_REPLY.to_vec(),
            TerminalQuery::SecondaryDeviceAttributes => format!(
                "\x1b[>{};{};{}c",
                self.da2_terminal_id, self.da2_version, self.da2_rom
            )
            .into_bytes(),
            TerminalQuery::DecModeReport { mode } => {
                let status = context
                    .modes
                    .and_then(|modes| decrpm_mode_enabled(modes, mode))
                    .map_or(0_u8, |enabled| if enabled { 1 } else { 2 });
                format!("\x1b[?{};{}$y", mode, status).into_bytes()
            }
        }
    }

    /// Decode and answer a parser action when it is a supported query.
    #[must_use]
    pub fn reply_for_action(self, action: &Action, context: ReplyContext<'_>) -> Option<Vec<u8>> {
        Self::query_from_action(action).map(|query| self.reply_for_query(query, context))
    }
}

/// Parse a terminal query sequence.
#[must_use]
pub fn parse_terminal_query(seq: &[u8]) -> Option<TerminalQuery> {
    TerminalQuery::parse_escape(seq)
}

/// Generate reply bytes for a parsed query using a default xterm-like identity.
#[must_use]
pub fn reply_for_query(query: TerminalQuery, cursor: &Cursor, modes: &Modes) -> Vec<u8> {
    ReplyEngine::default().reply_for_query(
        query,
        ReplyContext {
            cursor_row: cursor.row,
            cursor_col: cursor.col,
            modes: Some(modes),
        },
    )
}

/// Parse and answer a terminal query sequence.
#[must_use]
pub fn reply_for_query_bytes(seq: &[u8], cursor: &Cursor, modes: &Modes) -> Option<Vec<u8>> {
    parse_terminal_query(seq).map(|query| reply_for_query(query, cursor, modes))
}

fn parse_dsr_query(params: &[u8]) -> Option<TerminalQuery> {
    match params {
        b"5" => Some(TerminalQuery::DeviceStatus),
        b"6" => Some(TerminalQuery::CursorPosition),
        b"?6" => Some(TerminalQuery::ExtendedCursorPosition),
        _ => None,
    }
}

fn parse_da_query(params: &[u8]) -> Option<TerminalQuery> {
    match params {
        b"" | b"0" => Some(TerminalQuery::PrimaryDeviceAttributes),
        b">" | b">0" => Some(TerminalQuery::SecondaryDeviceAttributes),
        _ => None,
    }
}

fn parse_decrpm_query(params: &[u8]) -> Option<TerminalQuery> {
    let payload = params.strip_prefix(b"?")?;
    let mode_bytes = payload.strip_suffix(b"$")?;
    let mode = parse_u16_ascii(mode_bytes)?;
    Some(TerminalQuery::DecModeReport { mode })
}

fn parse_u16_ascii(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_u32;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value.saturating_mul(10).saturating_add(u32::from(b - b'0'));
        if value > u32::from(u16::MAX) {
            return None;
        }
    }
    Some(value as u16)
}

fn decrpm_mode_enabled(modes: &Modes, mode: u16) -> Option<bool> {
    let enabled = match mode {
        1 => modes.dec_flags().contains(DecModes::APPLICATION_CURSOR),
        6 => modes.origin_mode(),
        7 => modes.autowrap(),
        25 => modes.cursor_visible(),
        1000 => modes.dec_flags().contains(DecModes::MOUSE_BUTTON),
        1002 => modes.dec_flags().contains(DecModes::MOUSE_CELL_MOTION),
        1003 => modes.dec_flags().contains(DecModes::MOUSE_ALL_MOTION),
        1004 => modes.focus_events(),
        1006 => modes.dec_flags().contains(DecModes::MOUSE_SGR),
        1049 => modes.alt_screen(),
        2004 => modes.bracketed_paste(),
        2026 => modes.sync_output(),
        _ => return None,
    };
    Some(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    #[test]
    fn parses_supported_queries() {
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[5n"),
            Some(TerminalQuery::DeviceStatus)
        );
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[6n"),
            Some(TerminalQuery::CursorPosition)
        );
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[?6n"),
            Some(TerminalQuery::ExtendedCursorPosition)
        );
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[c"),
            Some(TerminalQuery::PrimaryDeviceAttributes)
        );
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[0c"),
            Some(TerminalQuery::PrimaryDeviceAttributes)
        );
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[>c"),
            Some(TerminalQuery::SecondaryDeviceAttributes)
        );
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[?2026$p"),
            Some(TerminalQuery::DecModeReport { mode: 2026 })
        );
    }

    #[test]
    fn ignores_unsupported_queries() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[?1;2c"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[4n"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[?foo$p"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b]0;title\x07"), None);
    }

    #[test]
    fn encodes_dsr_and_da_replies() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 4,
            cursor_col: 9,
            modes: None,
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DeviceStatus, context),
            b"\x1b[0n"
        );
        assert_eq!(
            engine.reply_for_query(TerminalQuery::CursorPosition, context),
            b"\x1b[5;10R"
        );
        assert_eq!(
            engine.reply_for_query(TerminalQuery::PrimaryDeviceAttributes, context),
            b"\x1b[?64;1;2;4;6;9;15;18;21;22c"
        );
        assert_eq!(
            engine.reply_for_query(TerminalQuery::SecondaryDeviceAttributes, context),
            b"\x1b[>1;10;0c"
        );
    }

    #[test]
    fn encodes_decrpm_from_modes() {
        let engine = ReplyEngine::default();
        let mut modes = Modes::new();
        modes.set_dec_mode(2026, true);
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 2026 }, context),
            b"\x1b[?2026;1$y"
        );
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 25 }, context),
            b"\x1b[?25;1$y"
        );
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 1004 }, context),
            b"\x1b[?1004;2$y"
        );
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 9999 }, context),
            b"\x1b[?9999;0$y"
        );
    }

    #[test]
    fn reply_for_action_uses_escape_actions() {
        let mut parser = Parser::new();
        let actions = parser.feed(b"\x1b[6n\x1b[?2026$p");
        assert_eq!(actions.len(), 2);
        let mut modes = Modes::new();
        modes.set_dec_mode(2026, true);
        let context = ReplyContext {
            cursor_row: 2,
            cursor_col: 7,
            modes: Some(&modes),
        };
        let engine = ReplyEngine::default();
        assert_eq!(
            engine.reply_for_action(&actions[0], context),
            Some(b"\x1b[3;8R".to_vec())
        );
        assert_eq!(
            engine.reply_for_action(&actions[1], context),
            Some(b"\x1b[?2026;1$y".to_vec())
        );
    }

    #[test]
    fn wrapper_api_roundtrips_queries() {
        let mut cursor = Cursor::new(100, 50);
        cursor.row = 11;
        cursor.col = 34;
        let mut modes = Modes::new();
        modes.set_dec_mode(2026, true);

        assert_eq!(
            parse_terminal_query(b"\x1b[?6n"),
            Some(TerminalQuery::ExtendedCursorPosition)
        );
        assert_eq!(
            reply_for_query_bytes(b"\x1b[?6n", &cursor, &modes),
            Some(b"\x1b[?12;35R".to_vec())
        );
        assert_eq!(
            reply_for_query_bytes(b"\x1b[?2026$p", &cursor, &modes),
            Some(b"\x1b[?2026;1$y".to_vec())
        );
    }
}
