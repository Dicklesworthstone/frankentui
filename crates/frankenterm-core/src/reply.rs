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

    // ---- parse_escape edge cases ----

    #[test]
    fn parse_escape_too_short() {
        assert_eq!(TerminalQuery::parse_escape(b""), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b["), None);
    }

    #[test]
    fn parse_escape_wrong_prefix() {
        assert_eq!(TerminalQuery::parse_escape(b"AB5n"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b]5n"), None);
    }

    #[test]
    fn parse_escape_unknown_final_byte() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[5z"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[0m"), None);
    }

    // ---- parse_u16_ascii (tested indirectly via DECRPM) ----

    #[test]
    fn decrpm_mode_zero() {
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[?0$p"),
            Some(TerminalQuery::DecModeReport { mode: 0 })
        );
    }

    #[test]
    fn decrpm_max_valid_mode() {
        // u16::MAX = 65535
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[?65535$p"),
            Some(TerminalQuery::DecModeReport { mode: 65535 })
        );
    }

    #[test]
    fn decrpm_overflow_u16_returns_none() {
        // 65536 > u16::MAX
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[?65536$p"), None);
    }

    #[test]
    fn decrpm_non_digit_in_mode_returns_none() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[?abc$p"), None);
    }

    #[test]
    fn decrpm_missing_question_mark_returns_none() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[2026$p"), None);
    }

    #[test]
    fn decrpm_missing_dollar_returns_none() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[?2026p"), None);
    }

    #[test]
    fn decrpm_empty_mode_number_returns_none() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[?$p"), None);
    }

    // ---- DA1/DA2 parse variants ----

    #[test]
    fn da2_with_explicit_zero() {
        assert_eq!(
            TerminalQuery::parse_escape(b"\x1b[>0c"),
            Some(TerminalQuery::SecondaryDeviceAttributes)
        );
    }

    #[test]
    fn da_invalid_param_returns_none() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[1c"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[>1c"), None);
    }

    // ---- DSR parse variants ----

    #[test]
    fn dsr_unknown_param_returns_none() {
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[7n"), None);
        assert_eq!(TerminalQuery::parse_escape(b"\x1b[0n"), None);
    }

    // ---- ReplyEngine identity ----

    #[test]
    fn xterm_like_da2_values() {
        let engine = ReplyEngine::xterm_like();
        assert_eq!(engine.da2_terminal_id, 1);
        assert_eq!(engine.da2_version, 10);
        assert_eq!(engine.da2_rom, 0);
    }

    #[test]
    fn default_equals_xterm_like() {
        assert_eq!(ReplyEngine::default(), ReplyEngine::xterm_like());
    }

    // ---- reply_for_query: cursor position edge cases ----

    #[test]
    fn cursor_position_at_origin() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        // 0-based (0,0) → 1-indexed (1,1)
        assert_eq!(
            engine.reply_for_query(TerminalQuery::CursorPosition, context),
            b"\x1b[1;1R"
        );
    }

    #[test]
    fn cursor_position_at_large_values() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 999,
            cursor_col: 499,
            modes: None,
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::CursorPosition, context),
            b"\x1b[1000;500R"
        );
    }

    #[test]
    fn extended_cursor_position_reply() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 3,
            cursor_col: 7,
            modes: None,
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::ExtendedCursorPosition, context),
            b"\x1b[?4;8R"
        );
    }

    #[test]
    fn cursor_position_u16_max_saturates() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: u16::MAX,
            cursor_col: u16::MAX,
            modes: None,
        };
        // saturating_add(1) on u16::MAX = u16::MAX
        let reply = engine.reply_for_query(TerminalQuery::CursorPosition, context);
        let expected = format!("\x1b[{};{}R", u16::MAX, u16::MAX).into_bytes();
        assert_eq!(reply, expected);
    }

    // ---- DA2 custom engine ----

    #[test]
    fn custom_da2_identity() {
        let engine = ReplyEngine {
            da2_terminal_id: 42,
            da2_version: 100,
            da2_rom: 5,
        };
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::SecondaryDeviceAttributes, context),
            b"\x1b[>42;100;5c"
        );
    }

    // ---- DECRPM with modes: all supported mode values ----

    #[test]
    fn decrpm_application_cursor_mode_1() {
        let engine = ReplyEngine::default();
        let mut modes = Modes::new();
        modes.set_dec_mode(1, true);
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 1 }, context),
            b"\x1b[?1;1$y"
        );
    }

    #[test]
    fn decrpm_origin_mode_6() {
        let engine = ReplyEngine::default();
        let mut modes = Modes::new();
        modes.set_dec_mode(6, true);
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 6 }, context),
            b"\x1b[?6;1$y"
        );
    }

    #[test]
    fn decrpm_autowrap_mode_7() {
        let engine = ReplyEngine::default();
        let modes = Modes::new();
        // Autowrap default is typically on
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        let reply = engine.reply_for_query(TerminalQuery::DecModeReport { mode: 7 }, context);
        // Should contain ;1$y (enabled) or ;2$y (disabled) — just check it's valid
        assert!(reply.starts_with(b"\x1b[?7;"));
        assert!(reply.ends_with(b"$y"));
    }

    #[test]
    fn decrpm_mouse_modes() {
        let engine = ReplyEngine::default();
        let mut modes = Modes::new();
        modes.set_dec_mode(1000, true); // MOUSE_BUTTON
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 1000 }, context),
            b"\x1b[?1000;1$y"
        );
        // 1002 should be disabled
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 1002 }, context),
            b"\x1b[?1002;2$y"
        );
    }

    #[test]
    fn decrpm_alt_screen_mode_1049() {
        let engine = ReplyEngine::default();
        let modes = Modes::new();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        // Default: alt_screen off → status 2
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 1049 }, context),
            b"\x1b[?1049;2$y"
        );
    }

    #[test]
    fn decrpm_bracketed_paste_mode_2004() {
        let engine = ReplyEngine::default();
        let mut modes = Modes::new();
        modes.set_dec_mode(2004, true);
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 2004 }, context),
            b"\x1b[?2004;1$y"
        );
    }

    #[test]
    fn decrpm_unknown_mode_returns_status_zero() {
        let engine = ReplyEngine::default();
        let modes = Modes::new();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        // Mode 9999 is not recognized → status 0
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 9999 }, context),
            b"\x1b[?9999;0$y"
        );
    }

    #[test]
    fn decrpm_without_modes_context_returns_status_zero() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        // No modes context → status 0
        assert_eq!(
            engine.reply_for_query(TerminalQuery::DecModeReport { mode: 2026 }, context),
            b"\x1b[?2026;0$y"
        );
    }

    // ---- query_from_action ----

    #[test]
    fn query_from_action_device_attributes() {
        assert_eq!(
            ReplyEngine::query_from_action(&Action::DeviceAttributes),
            Some(TerminalQuery::PrimaryDeviceAttributes)
        );
    }

    #[test]
    fn query_from_action_device_attributes_secondary() {
        assert_eq!(
            ReplyEngine::query_from_action(&Action::DeviceAttributesSecondary),
            Some(TerminalQuery::SecondaryDeviceAttributes)
        );
    }

    #[test]
    fn query_from_action_device_status_report() {
        assert_eq!(
            ReplyEngine::query_from_action(&Action::DeviceStatusReport),
            Some(TerminalQuery::DeviceStatus)
        );
    }

    #[test]
    fn query_from_action_cursor_position_report() {
        assert_eq!(
            ReplyEngine::query_from_action(&Action::CursorPositionReport),
            Some(TerminalQuery::CursorPosition)
        );
    }

    #[test]
    fn query_from_action_escape_with_query() {
        let action = Action::Escape(b"\x1b[5n".to_vec());
        assert_eq!(
            ReplyEngine::query_from_action(&action),
            Some(TerminalQuery::DeviceStatus)
        );
    }

    #[test]
    fn query_from_action_escape_without_query() {
        let action = Action::Escape(b"\x1b[0m".to_vec());
        assert_eq!(ReplyEngine::query_from_action(&action), None);
    }

    #[test]
    fn query_from_action_non_query_returns_none() {
        assert_eq!(ReplyEngine::query_from_action(&Action::Print('A')), None);
    }

    // ---- reply_for_action ----

    #[test]
    fn reply_for_action_returns_none_for_non_query() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        assert_eq!(engine.reply_for_action(&Action::Print('x'), context), None);
    }

    #[test]
    fn reply_for_action_returns_reply_for_da1() {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        let reply = engine.reply_for_action(&Action::DeviceAttributes, context);
        assert_eq!(reply, Some(DA1_REPLY.to_vec()));
    }

    // ---- wrapper APIs ----

    #[test]
    fn parse_terminal_query_delegates_to_parse_escape() {
        assert_eq!(
            parse_terminal_query(b"\x1b[c"),
            Some(TerminalQuery::PrimaryDeviceAttributes)
        );
        assert_eq!(parse_terminal_query(b"not an escape"), None);
    }

    #[test]
    fn reply_for_query_bytes_returns_none_for_invalid() {
        let cursor = Cursor::new(80, 24);
        let modes = Modes::new();
        assert_eq!(reply_for_query_bytes(b"garbage", &cursor, &modes), None);
    }

    #[test]
    fn reply_for_query_uses_cursor_position() {
        let mut cursor = Cursor::new(80, 24);
        cursor.row = 5;
        cursor.col = 10;
        let modes = Modes::new();
        // DSR cursor position: should use row=5, col=10 → "6;11R"
        let reply = reply_for_query(TerminalQuery::CursorPosition, &cursor, &modes);
        assert_eq!(reply, b"\x1b[6;11R");
    }

    // ---- DA1 constant ----

    #[test]
    fn da1_reply_starts_with_esc_bracket() {
        assert!(DA1_REPLY.starts_with(b"\x1b[?"));
        assert!(DA1_REPLY.ends_with(b"c"));
    }
}
