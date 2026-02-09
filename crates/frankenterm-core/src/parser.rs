//! VT/ANSI parser.
//!
//! This parser is a deterministic state machine that converts an output byte
//! stream into a sequence of actions for the terminal engine. It covers:
//!
//! - printable characters (ASCII + full UTF-8) -> `Action::Print`
//! - C0 controls -> dedicated actions
//! - CSI sequences (cursor, erase, scroll, SGR, mode set/reset)
//! - OSC sequences (title, hyperlinks)
//! - ESC-level sequences (cursor save/restore, index, reset)
//! - capture of unsupported sequences as `Action::Escape` for later decoding

/// Parser output actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print a single character (ASCII or multi-byte UTF-8).
    Print(char),
    /// Line feed / newline (`\n`).
    Newline,
    /// Carriage return (`\r`).
    CarriageReturn,
    /// Horizontal tab (`\t`).
    Tab,
    /// Backspace (`\x08`).
    Backspace,
    /// Bell (`\x07`).
    Bell,
    /// CUU (`CSI Ps A`): move cursor up by count (default 1).
    CursorUp(u16),
    /// CUD (`CSI Ps B`): move cursor down by count (default 1).
    CursorDown(u16),
    /// CUF (`CSI Ps C`): move cursor right by count (default 1).
    CursorRight(u16),
    /// CUB (`CSI Ps D`): move cursor left by count (default 1).
    CursorLeft(u16),
    /// CNL (`CSI Ps E`): move cursor down by count and to column 0.
    CursorNextLine(u16),
    /// CPL (`CSI Ps F`): move cursor up by count and to column 0.
    CursorPrevLine(u16),
    /// CHA (`CSI Ps G`): move cursor to absolute column (0-indexed).
    CursorColumn(u16),
    /// VPA (`CSI Ps d`): move cursor to absolute row (0-indexed).
    CursorRow(u16),
    /// DECSTBM (`CSI Pt ; Pb r`): set scrolling region. `bottom == 0` means
    /// "use full height" (default), since the parser does not know the grid size.
    ///
    /// `top` is 0-indexed inclusive. `bottom` is 0-indexed exclusive when non-zero.
    SetScrollRegion { top: u16, bottom: u16 },
    /// SU (`CSI Ps S`): scroll the scroll region up by count (default 1).
    ScrollUp(u16),
    /// SD (`CSI Ps T`): scroll the scroll region down by count (default 1).
    ScrollDown(u16),
    /// IL (`CSI Ps L`): insert blank lines at cursor row within scroll region.
    InsertLines(u16),
    /// DL (`CSI Ps M`): delete lines at cursor row within scroll region.
    DeleteLines(u16),
    /// ICH (`CSI Ps @`): insert blank cells at cursor column.
    InsertChars(u16),
    /// DCH (`CSI Ps P`): delete cells at cursor column.
    DeleteChars(u16),
    /// CUP/HVP: move cursor to absolute 0-indexed row/col.
    CursorPosition { row: u16, col: u16 },
    /// ED mode (`CSI Ps J`): 0, 1, or 2.
    EraseInDisplay(u8),
    /// EL mode (`CSI Ps K`): 0, 1, or 2.
    EraseInLine(u8),
    /// SGR (`CSI ... m`): set graphics rendition parameters (attributes/colors).
    ///
    /// Parameters are returned as parsed numeric values; interpretation is
    /// performed by the terminal engine (they are stateful/delta-based).
    Sgr(Vec<u16>),
    /// DECSET (`CSI ? Pm h`): enable DEC private mode(s).
    DecSet(Vec<u16>),
    /// DECRST (`CSI ? Pm l`): disable DEC private mode(s).
    DecRst(Vec<u16>),
    /// SM (`CSI Pm h`): enable ANSI standard mode(s).
    AnsiSet(Vec<u16>),
    /// RM (`CSI Pm l`): disable ANSI standard mode(s).
    AnsiRst(Vec<u16>),
    /// DECSC (`ESC 7`): save cursor state.
    SaveCursor,
    /// DECRC (`ESC 8`): restore cursor state.
    RestoreCursor,
    /// IND (`ESC D`): index — move cursor down one line, scrolling if at bottom.
    Index,
    /// RI (`ESC M`): reverse index — move cursor up one line, scrolling if at top.
    ReverseIndex,
    /// NEL (`ESC E`): next line — move cursor to start of next line.
    NextLine,
    /// RIS (`ESC c`): full reset to initial state.
    FullReset,
    /// OSC 0/2: set terminal title.
    SetTitle(String),
    /// OSC 8: start a hyperlink with the given URI.
    HyperlinkStart(String),
    /// OSC 8: end the current hyperlink.
    HyperlinkEnd,
    /// A raw escape/CSI/OSC sequence captured verbatim (starts with ESC).
    Escape(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    /// Accumulating a multi-byte UTF-8 character.
    /// `bytes_remaining` counts how many continuation bytes are still expected.
    Utf8 {
        bytes_remaining: u8,
    },
}

/// VT/ANSI parser state.
#[derive(Debug, Clone)]
pub struct Parser {
    state: State,
    buf: Vec<u8>,
    /// Accumulator for multi-byte UTF-8 character assembly.
    utf8_buf: [u8; 4],
    /// Number of bytes accumulated so far in `utf8_buf`.
    utf8_len: u8,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// Create a new parser in ground state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            buf: Vec::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
        }
    }

    /// Feed a chunk of bytes and return parsed actions.
    #[must_use]
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut out = Vec::new();
        for &b in bytes {
            if let Some(action) = self.advance(b) {
                out.push(action);
            }
        }
        out
    }

    /// Advance the parser by one byte.
    ///
    /// Returns an action when a complete token is recognized.
    pub fn advance(&mut self, b: u8) -> Option<Action> {
        match self.state {
            State::Ground => self.advance_ground(b),
            State::Esc => self.advance_esc(b),
            State::Csi => self.advance_csi(b),
            State::Osc => self.advance_osc(b),
            State::OscEsc => self.advance_osc_esc(b),
            State::Utf8 { bytes_remaining } => self.advance_utf8(b, bytes_remaining),
        }
    }

    fn advance_ground(&mut self, b: u8) -> Option<Action> {
        match b {
            b'\n' | 0x0B | 0x0C => Some(Action::Newline), // LF, VT, FF all treated as newline
            b'\r' => Some(Action::CarriageReturn),
            b'\t' => Some(Action::Tab),
            0x08 => Some(Action::Backspace),
            0x07 => Some(Action::Bell),
            0x1b => {
                self.state = State::Esc;
                self.buf.clear();
                self.buf.push(0x1b);
                None
            }
            0x20..=0x7E => Some(Action::Print(b as char)),
            // UTF-8 multi-byte sequence leading bytes:
            0xC2..=0xDF => {
                // 2-byte sequence (0xC0-0xC1 are overlong, rejected)
                self.utf8_buf[0] = b;
                self.utf8_len = 1;
                self.state = State::Utf8 { bytes_remaining: 1 };
                None
            }
            0xE0..=0xEF => {
                // 3-byte sequence
                self.utf8_buf[0] = b;
                self.utf8_len = 1;
                self.state = State::Utf8 { bytes_remaining: 2 };
                None
            }
            0xF0..=0xF4 => {
                // 4-byte sequence (0xF5-0xF7 are outside valid Unicode range)
                self.utf8_buf[0] = b;
                self.utf8_len = 1;
                self.state = State::Utf8 { bytes_remaining: 3 };
                None
            }
            _ => None, // ignore C0 controls (0x00-0x06, 0x0E-0x1A, 0x1C-0x1F)
                       // and invalid UTF-8 leading bytes (0x80-0xBF, 0xC0-0xC1, 0xF5-0xFF)
        }
    }

    /// Accumulate continuation bytes for a multi-byte UTF-8 character.
    fn advance_utf8(&mut self, b: u8, bytes_remaining: u8) -> Option<Action> {
        // Continuation bytes must be in 0x80..=0xBF.
        if (0x80..=0xBF).contains(&b) {
            let idx = self.utf8_len as usize;
            if idx < 4 {
                self.utf8_buf[idx] = b;
                self.utf8_len += 1;
            }
            if bytes_remaining == 1 {
                // Sequence complete — try to decode.
                self.state = State::Ground;
                let len = self.utf8_len as usize;
                let ch = core::str::from_utf8(&self.utf8_buf[..len])
                    .ok()
                    .and_then(|s| s.chars().next());
                self.utf8_len = 0;
                ch.map(Action::Print)
            } else {
                self.state = State::Utf8 {
                    bytes_remaining: bytes_remaining - 1,
                };
                None
            }
        } else {
            // Invalid continuation byte — abort UTF-8, reprocess this byte
            // in ground state (replacement character is omitted per VT semantics;
            // terminal emulators typically drop malformed sequences).
            self.state = State::Ground;
            self.utf8_len = 0;
            self.advance_ground(b)
        }
    }

    fn advance_esc(&mut self, b: u8) -> Option<Action> {
        self.buf.push(b);
        match b {
            b'[' => {
                self.state = State::Csi;
                None
            }
            b']' => {
                self.state = State::Osc;
                None
            }
            // DECSC: save cursor (ESC 7)
            b'7' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::SaveCursor)
            }
            // DECRC: restore cursor (ESC 8)
            b'8' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::RestoreCursor)
            }
            // IND: index — cursor down, scroll if at bottom margin (ESC D)
            b'D' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::Index)
            }
            // RI: reverse index — cursor up, scroll if at top margin (ESC M)
            b'M' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::ReverseIndex)
            }
            // NEL: next line — CR + LF (ESC E)
            b'E' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::NextLine)
            }
            // RIS: full reset to initial state (ESC c)
            b'c' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::FullReset)
            }
            _ => {
                self.state = State::Ground;
                Some(Action::Escape(self.take_buf()))
            }
        }
    }

    fn advance_csi(&mut self, b: u8) -> Option<Action> {
        self.buf.push(b);
        // Final byte for CSI is in the 0x40..=0x7E range (ECMA-48).
        if (0x40..=0x7E).contains(&b) {
            self.state = State::Ground;
            let seq = self.take_buf();
            return Some(Self::decode_csi(&seq).unwrap_or(Action::Escape(seq)));
        }
        None
    }

    fn advance_osc(&mut self, b: u8) -> Option<Action> {
        self.buf.push(b);
        match b {
            0x07 => {
                // BEL terminator.
                self.state = State::Ground;
                let seq = self.take_buf();
                Some(Self::decode_osc(&seq).unwrap_or(Action::Escape(seq)))
            }
            0x1b => {
                // ESC, possibly starting ST terminator (ESC \).
                self.state = State::OscEsc;
                None
            }
            _ => None,
        }
    }

    fn advance_osc_esc(&mut self, b: u8) -> Option<Action> {
        self.buf.push(b);
        if b == b'\\' {
            // ST terminator.
            self.state = State::Ground;
            let seq = self.take_buf();
            return Some(Self::decode_osc(&seq).unwrap_or(Action::Escape(seq)));
        }
        // False alarm; continue OSC.
        self.state = State::Osc;
        None
    }

    fn take_buf(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        core::mem::swap(&mut out, &mut self.buf);
        out
    }

    fn decode_csi(seq: &[u8]) -> Option<Action> {
        if seq.len() < 3 || seq[0] != 0x1b || seq[1] != b'[' {
            return None;
        }
        let final_byte = *seq.last()?;
        let param_bytes = &seq[2..seq.len().saturating_sub(1)];

        // Check for DEC private mode indicator `?` prefix.
        if param_bytes.first() == Some(&b'?') {
            let params = Self::parse_csi_params(&param_bytes[1..])?;
            return match final_byte {
                b'h' => Some(Action::DecSet(params)),
                b'l' => Some(Action::DecRst(params)),
                _ => None,
            };
        }

        let params = Self::parse_csi_params(param_bytes)?;

        match final_byte {
            b'A' => Some(Action::CursorUp(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'B' => Some(Action::CursorDown(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'C' => Some(Action::CursorRight(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'D' => Some(Action::CursorLeft(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'E' => Some(Action::CursorNextLine(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'F' => Some(Action::CursorPrevLine(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'G' => Some(Action::CursorColumn(
                Self::csi_count_or_one(params.first().copied()).saturating_sub(1),
            )),
            b'H' | b'f' => {
                // CUP/HVP use 1-indexed coordinates; 0 is treated as 1.
                let row = params
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_sub(1);
                let col = params.get(1).copied().unwrap_or(1).max(1).saturating_sub(1);
                Some(Action::CursorPosition { row, col })
            }
            b'J' => {
                let mode = params.first().copied().unwrap_or(0);
                if mode <= 2 {
                    Some(Action::EraseInDisplay(mode as u8))
                } else {
                    None
                }
            }
            b'K' => {
                let mode = params.first().copied().unwrap_or(0);
                if mode <= 2 {
                    Some(Action::EraseInLine(mode as u8))
                } else {
                    None
                }
            }
            b'd' => Some(Action::CursorRow(
                Self::csi_count_or_one(params.first().copied()).saturating_sub(1),
            )),
            b'L' => Some(Action::InsertLines(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'M' => Some(Action::DeleteLines(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'@' => Some(Action::InsertChars(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'P' => Some(Action::DeleteChars(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'S' => Some(Action::ScrollUp(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'T' => Some(Action::ScrollDown(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            b'r' => {
                let top = params
                    .first()
                    .copied()
                    .unwrap_or(0)
                    .max(1)
                    .saturating_sub(1);
                let bottom = params.get(1).copied().unwrap_or(0);
                Some(Action::SetScrollRegion { top, bottom })
            }
            b'm' => Some(Action::Sgr(params)),
            // SM: set ANSI mode(s)
            b'h' => Some(Action::AnsiSet(params)),
            // RM: reset ANSI mode(s)
            b'l' => Some(Action::AnsiRst(params)),
            _ => None,
        }
    }

    fn decode_osc(seq: &[u8]) -> Option<Action> {
        if seq.len() < 4 || seq[0] != 0x1b || seq[1] != b']' {
            return None;
        }

        // Strip terminator (BEL or ST).
        let content = if *seq.last()? == 0x07 {
            &seq[2..seq.len().saturating_sub(1)]
        } else if seq.len() >= 4 && seq[seq.len() - 2] == 0x1b && seq[seq.len() - 1] == b'\\' {
            &seq[2..seq.len().saturating_sub(2)]
        } else {
            return None;
        };

        let first_semi = content.iter().position(|&b| b == b';')?;
        let cmd = core::str::from_utf8(&content[..first_semi]).ok()?;
        let cmd: u16 = cmd.parse().ok()?;
        let rest = &content[first_semi + 1..];

        match cmd {
            0 | 2 => {
                let title = String::from_utf8_lossy(rest).to_string();
                Some(Action::SetTitle(title))
            }
            8 => {
                // OSC 8 ; params ; uri ST/BEL
                let second_semi = rest.iter().position(|&b| b == b';')?;
                let uri = &rest[second_semi + 1..];
                if uri.is_empty() {
                    Some(Action::HyperlinkEnd)
                } else {
                    Some(Action::HyperlinkStart(
                        String::from_utf8_lossy(uri).to_string(),
                    ))
                }
            }
            _ => None,
        }
    }

    fn parse_csi_params(params: &[u8]) -> Option<Vec<u16>> {
        if params.is_empty() {
            return Some(Vec::new());
        }
        let s = core::str::from_utf8(params).ok()?;
        let mut out = Vec::new();
        for part in s.split(';') {
            if part.is_empty() {
                out.push(0);
                continue;
            }
            let value = part.parse::<u32>().ok()?;
            out.push(value.min(u16::MAX as u32) as u16);
        }
        Some(out)
    }

    fn csi_count_or_one(value: Option<u16>) -> u16 {
        value.unwrap_or(1).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ASCII / Ground ─────────────────────────────────────────────

    #[test]
    fn printable_ascii_emits_print() {
        let mut p = Parser::new();
        let actions = p.feed(b"hi");
        assert_eq!(actions, vec![Action::Print('h'), Action::Print('i')]);
    }

    #[test]
    fn c0_controls_emit_actions() {
        let mut p = Parser::new();
        let actions = p.feed(b"\t\r\n");
        assert_eq!(
            actions,
            vec![Action::Tab, Action::CarriageReturn, Action::Newline]
        );
    }

    #[test]
    fn vt_and_ff_treated_as_newline() {
        let mut p = Parser::new();
        // VT (0x0B) and FF (0x0C) both produce Newline
        assert_eq!(p.feed(b"\x0b"), vec![Action::Newline]);
        assert_eq!(p.feed(b"\x0c"), vec![Action::Newline]);
    }

    // ── UTF-8 multi-byte characters ────────────────────────────────

    #[test]
    fn utf8_two_byte_character() {
        let mut p = Parser::new();
        // é = U+00E9 = 0xC3 0xA9
        let actions = p.feed("é".as_bytes());
        assert_eq!(actions, vec![Action::Print('é')]);
    }

    #[test]
    fn utf8_three_byte_character() {
        let mut p = Parser::new();
        // 中 = U+4E2D = 0xE4 0xB8 0xAD
        let actions = p.feed("中".as_bytes());
        assert_eq!(actions, vec![Action::Print('中')]);
    }

    #[test]
    fn utf8_four_byte_character() {
        let mut p = Parser::new();
        // 🎉 = U+1F389 = 0xF0 0x9F 0x8E 0x89
        let actions = p.feed("🎉".as_bytes());
        assert_eq!(actions, vec![Action::Print('🎉')]);
    }

    #[test]
    fn utf8_mixed_with_ascii() {
        let mut p = Parser::new();
        let actions = p.feed("aé中🎉b".as_bytes());
        assert_eq!(
            actions,
            vec![
                Action::Print('a'),
                Action::Print('é'),
                Action::Print('中'),
                Action::Print('🎉'),
                Action::Print('b'),
            ]
        );
    }

    #[test]
    fn utf8_split_across_feeds() {
        let mut p = Parser::new();
        // Feed é (0xC3 0xA9) byte by byte
        assert_eq!(p.feed(&[0xC3]), Vec::<Action>::new());
        assert_eq!(p.feed(&[0xA9]), vec![Action::Print('é')]);
    }

    #[test]
    fn utf8_split_four_byte_across_feeds() {
        let mut p = Parser::new();
        // 🎉 = 0xF0 0x9F 0x8E 0x89
        assert!(p.feed(&[0xF0]).is_empty());
        assert!(p.feed(&[0x9F]).is_empty());
        assert!(p.feed(&[0x8E]).is_empty());
        assert_eq!(p.feed(&[0x89]), vec![Action::Print('🎉')]);
    }

    #[test]
    fn utf8_invalid_continuation_aborts_and_reprocesses() {
        let mut p = Parser::new();
        // Start a 2-byte sequence (0xC3) then send ASCII 'a' instead of continuation
        let actions = p.feed(&[0xC3, b'a']);
        // The invalid sequence is dropped, 'a' is reprocessed as ASCII
        assert_eq!(actions, vec![Action::Print('a')]);
    }

    #[test]
    fn utf8_overlong_leading_bytes_are_ignored() {
        let mut p = Parser::new();
        // 0xC0 and 0xC1 are overlong leading bytes — should be ignored
        assert!(p.feed(&[0xC0]).is_empty());
        assert!(p.feed(&[0xC1]).is_empty());
    }

    #[test]
    fn utf8_invalid_leading_bytes_above_f4_ignored() {
        let mut p = Parser::new();
        // 0xF5-0xFF are above valid Unicode range
        assert!(p.feed(&[0xF5]).is_empty());
        assert!(p.feed(&[0xFF]).is_empty());
    }

    #[test]
    fn utf8_interrupted_by_escape() {
        let mut p = Parser::new();
        // Start UTF-8, then get ESC — should abort UTF-8 and process ESC
        let actions = p.feed(&[0xC3, 0x1b, b'c']);
        // 0xC3 starts UTF-8, 0x1b is not a valid continuation so abort,
        // reprocess 0x1b as ESC, then 'c' completes ESC c -> FullReset
        assert_eq!(actions, vec![Action::FullReset]);
    }

    #[test]
    fn utf8_japanese_text() {
        let mut p = Parser::new();
        let actions = p.feed("こんにちは".as_bytes());
        assert_eq!(
            actions,
            vec![
                Action::Print('こ'),
                Action::Print('ん'),
                Action::Print('に'),
                Action::Print('ち'),
                Action::Print('は'),
            ]
        );
    }

    // ── DECSET / DECRST ────────────────────────────────────────────

    #[test]
    fn decset_cursor_hide() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[?25l");
        assert_eq!(actions, vec![Action::DecRst(vec![25])]);
    }

    #[test]
    fn decset_cursor_show() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[?25h");
        assert_eq!(actions, vec![Action::DecSet(vec![25])]);
    }

    #[test]
    fn decset_multiple_modes() {
        let mut p = Parser::new();
        // Enable alt screen + bracketed paste + mouse SGR in one sequence
        let actions = p.feed(b"\x1b[?1049;2004;1006h");
        assert_eq!(actions, vec![Action::DecSet(vec![1049, 2004, 1006])]);
    }

    #[test]
    fn decrst_multiple_modes() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[?1049;2004l");
        assert_eq!(actions, vec![Action::DecRst(vec![1049, 2004])]);
    }

    #[test]
    fn decset_sync_output() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[?2026h"), vec![Action::DecSet(vec![2026])]);
        assert_eq!(p.feed(b"\x1b[?2026l"), vec![Action::DecRst(vec![2026])]);
    }

    #[test]
    fn decset_autowrap() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[?7h"), vec![Action::DecSet(vec![7])]);
        assert_eq!(p.feed(b"\x1b[?7l"), vec![Action::DecRst(vec![7])]);
    }

    // ── ANSI SM / RM ──────────────────────────────────────────────

    #[test]
    fn ansi_set_insert_mode() {
        let mut p = Parser::new();
        // SM (CSI 4 h) — set insert mode
        assert_eq!(p.feed(b"\x1b[4h"), vec![Action::AnsiSet(vec![4])]);
        // RM (CSI 4 l) — reset insert mode
        assert_eq!(p.feed(b"\x1b[4l"), vec![Action::AnsiRst(vec![4])]);
    }

    #[test]
    fn ansi_set_newline_mode() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[20h"), vec![Action::AnsiSet(vec![20])]);
        assert_eq!(p.feed(b"\x1b[20l"), vec![Action::AnsiRst(vec![20])]);
    }

    // ── Cursor save/restore (DECSC/DECRC) ──────────────────────────

    #[test]
    fn esc_7_saves_cursor() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b7"), vec![Action::SaveCursor]);
    }

    #[test]
    fn esc_8_restores_cursor() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b8"), vec![Action::RestoreCursor]);
    }

    #[test]
    fn save_restore_roundtrip_sequence() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b7\x1b[5;10H\x1b8");
        assert_eq!(
            actions,
            vec![
                Action::SaveCursor,
                Action::CursorPosition { row: 4, col: 9 },
                Action::RestoreCursor,
            ]
        );
    }

    // ── ESC-level sequences (IND, RI, NEL, RIS) ───────────────────

    #[test]
    fn esc_d_is_index() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bD"), vec![Action::Index]);
    }

    #[test]
    fn esc_m_is_reverse_index() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bM"), vec![Action::ReverseIndex]);
    }

    #[test]
    fn esc_e_is_next_line() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bE"), vec![Action::NextLine]);
    }

    #[test]
    fn esc_c_is_full_reset() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bc"), vec![Action::FullReset]);
    }

    // ── Original CSI tests (preserved) ────────────────────────────

    #[test]
    fn csi_sgr_is_decoded() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[31m"), vec![Action::Sgr(vec![31])]);
        assert_eq!(p.feed(b"\x1b[m"), vec![Action::Sgr(vec![])]);
    }

    #[test]
    fn csi_cup_is_decoded_to_cursor_position() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[5;10H");
        assert_eq!(
            actions,
            vec![Action::CursorPosition { row: 4, col: 9 }],
            "CUP should decode as 0-indexed cursor position"
        );

        let actions = p.feed(b"\x1b[0;0H");
        assert_eq!(
            actions,
            vec![Action::CursorPosition { row: 0, col: 0 }],
            "CUP zero params should default to 1;1"
        );
    }

    #[test]
    fn csi_ed_and_el_are_decoded() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[2J"), vec![Action::EraseInDisplay(2)]);
        assert_eq!(p.feed(b"\x1b[K"), vec![Action::EraseInLine(0)]);
    }

    #[test]
    fn csi_cursor_relative_moves_are_decoded() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b[2A\x1b[B\x1b[3C\x1b[0D"),
            vec![
                Action::CursorUp(2),
                Action::CursorDown(1),
                Action::CursorRight(3),
                Action::CursorLeft(1),
            ]
        );
    }

    #[test]
    fn csi_cha_is_decoded_to_absolute_column() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[5G"), vec![Action::CursorColumn(4)]);
        assert_eq!(p.feed(b"\x1b[0G"), vec![Action::CursorColumn(0)]);
    }

    #[test]
    fn csi_cnl_cpl_and_vpa_are_decoded() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b[2E\x1b[F\x1b[3d\x1b[0d\x1b[d"),
            vec![
                Action::CursorNextLine(2),
                Action::CursorPrevLine(1),
                Action::CursorRow(2),
                Action::CursorRow(0),
                Action::CursorRow(0),
            ]
        );
    }

    #[test]
    fn csi_scroll_region_and_insert_delete_are_decoded() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b[2;4r\x1b[r\x1b[2S\x1b[T\x1b[3L\x1b[M\x1b[4@\x1b[P"),
            vec![
                Action::SetScrollRegion { top: 1, bottom: 4 },
                Action::SetScrollRegion { top: 0, bottom: 0 },
                Action::ScrollUp(2),
                Action::ScrollDown(1),
                Action::InsertLines(3),
                Action::DeleteLines(1),
                Action::InsertChars(4),
                Action::DeleteChars(1),
            ]
        );
    }

    #[test]
    fn osc_sequence_bel_terminated_is_captured() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b]0;title\x07"),
            vec![Action::SetTitle("title".to_string())]
        );
        assert_eq!(
            p.feed(b"\x1b]2;hi\x1b\\"),
            vec![Action::SetTitle("hi".to_string())]
        );
    }

    #[test]
    fn osc8_hyperlink_is_decoded() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b]8;;https://example.com\x07"),
            vec![Action::HyperlinkStart("https://example.com".to_string())]
        );
        assert_eq!(p.feed(b"\x1b]8;;\x07"), vec![Action::HyperlinkEnd]);
        assert_eq!(
            p.feed(b"\x1b]8;;https://a.test\x1b\\"),
            vec![Action::HyperlinkStart("https://a.test".to_string())]
        );
        assert_eq!(p.feed(b"\x1b]8;;\x1b\\"), vec![Action::HyperlinkEnd]);
    }

    // ── Integration: mixed sequences ───────────────────────────────

    #[test]
    fn mixed_utf8_csi_osc_sequence() {
        let mut p = Parser::new();
        // "Hello" in Japanese, then set red, then move cursor
        let mut input = Vec::new();
        input.extend_from_slice("日本語".as_bytes());
        input.extend_from_slice(b"\x1b[31m");
        input.extend_from_slice(b"\x1b[5;1H");
        let actions = p.feed(&input);
        assert_eq!(
            actions,
            vec![
                Action::Print('日'),
                Action::Print('本'),
                Action::Print('語'),
                Action::Sgr(vec![31]),
                Action::CursorPosition { row: 4, col: 0 },
            ]
        );
    }

    #[test]
    fn typical_terminal_setup_sequence() {
        let mut p = Parser::new();
        // Typical terminal init: alt screen + bracketed paste + mouse + hide cursor
        let actions = p.feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1006h\x1b[?25l");
        assert_eq!(
            actions,
            vec![
                Action::DecSet(vec![1049]),
                Action::DecSet(vec![2004]),
                Action::DecSet(vec![1006]),
                Action::DecRst(vec![25]),
            ]
        );
    }

    #[test]
    fn typical_terminal_teardown_sequence() {
        let mut p = Parser::new();
        // Typical terminal cleanup: show cursor + disable mouse + disable bracketed paste + exit alt screen
        let actions = p.feed(b"\x1b[?25h\x1b[?1006l\x1b[?2004l\x1b[?1049l");
        assert_eq!(
            actions,
            vec![
                Action::DecSet(vec![25]),
                Action::DecRst(vec![1006]),
                Action::DecRst(vec![2004]),
                Action::DecRst(vec![1049]),
            ]
        );
    }
}
