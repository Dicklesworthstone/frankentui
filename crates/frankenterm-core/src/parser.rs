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

use smallvec::SmallVec;

/// Inline capacity for CSI parameter lists.
///
/// Most SGR/DECSET sequences carry ≤ 4 parameters; this avoids a heap
/// allocation for the common case while remaining transparent to consumers
/// via `Deref<Target = [u16]>`.
pub type CsiParams = SmallVec<[u16; 4]>;

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
    Sgr(CsiParams),
    /// DECSET (`CSI ? Pm h`): enable DEC private mode(s).
    DecSet(CsiParams),
    /// DECRST (`CSI ? Pm l`): disable DEC private mode(s).
    DecRst(CsiParams),
    /// SM (`CSI Pm h`): enable ANSI standard mode(s).
    AnsiSet(CsiParams),
    /// RM (`CSI Pm l`): disable ANSI standard mode(s).
    AnsiRst(CsiParams),
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
    /// HTS (`ESC H`): set a tab stop at the current cursor column.
    SetTabStop,
    /// TBC (`CSI Ps g`): tab clear. 0 = at cursor, 3 = all tab stops.
    ClearTabStop(u16),
    /// CBT (`CSI Ps Z`): cursor backward tabulation by count (default 1).
    BackTab(u16),
    /// DECKPAM (`ESC =`): application keypad mode.
    ApplicationKeypad,
    /// DECKPNM (`ESC >`): normal keypad mode.
    NormalKeypad,
    /// ECH (`CSI Ps X`): erase characters at cursor position (replace with blanks).
    EraseChars(u16),
    /// DECALN (`ESC # 8`): DEC screen alignment test — fill entire grid with 'E'.
    ScreenAlignment,
    /// REP (`CSI Ps b`): repeat the most recently printed graphic character Ps times.
    RepeatChar(u16),
    /// DECSCUSR (`CSI Ps SP q`): set cursor shape.
    ///
    /// 0 = default, 1 = blinking block, 2 = steady block,
    /// 3 = blinking underline, 4 = steady underline,
    /// 5 = blinking bar, 6 = steady bar.
    SetCursorShape(u8),
    /// DECSTR (`CSI ! p`): soft terminal reset.
    ///
    /// Resets most terminal state (modes, SGR, scroll region, cursor
    /// visibility, character sets) without clearing the screen or
    /// scrollback — unlike RIS (`ESC c`) which is a full reset.
    SoftReset,
    /// ED mode 3 (`CSI 3 J`): erase the entire display and scrollback buffer.
    EraseScrollback,
    /// Focus gained (`CSI I`): terminal window received focus.
    FocusIn,
    /// Focus lost (`CSI O`): terminal window lost focus.
    FocusOut,
    /// Bracketed paste start (`CSI 200 ~`).
    PasteStart,
    /// Bracketed paste end (`CSI 201 ~`).
    PasteEnd,
    /// DA1 (`CSI c` / `CSI 0 c`): request primary device attributes.
    DeviceAttributes,
    /// DA2 (`CSI > c` / `CSI > 0 c`): request secondary device attributes.
    DeviceAttributesSecondary,
    /// DSR (`CSI 5 n`): device status report — "are you OK?" query.
    DeviceStatusReport,
    /// CPR (`CSI 6 n`): cursor position report — request current cursor position.
    CursorPositionReport,
    /// Designate character set for slot G0..G3.
    ///
    /// `slot`: 0 = G0 (ESC (), 1 = G1 (ESC )), 2 = G2 (ESC *), 3 = G3 (ESC +).
    /// `charset`: b'B' = ASCII (USASCII), b'0' = DEC Special Graphics, etc.
    DesignateCharset { slot: u8, charset: u8 },
    /// SS2 (`ESC N`): single shift to G2 for the next printed character only.
    SingleShift2,
    /// SS3 (`ESC O`): single shift to G3 for the next printed character only.
    SingleShift3,
    /// Mouse event from SGR mode 1006 (`CSI < Pb ; Px ; Py M/m`) or
    /// legacy X10 mode 1000 (`CSI M Cb Cx Cy`).
    ///
    /// `button`: 0 = left, 1 = middle, 2 = right, 3 = release (legacy),
    /// 64 = scroll up, 65 = scroll down. Modifier bits: 4 = shift, 8 = meta, 16 = ctrl.
    /// `col`/`row`: 0-based cell coordinates.
    /// `pressed`: true for press/motion, false for release.
    MouseEvent {
        button: u16,
        col: u16,
        row: u16,
        pressed: bool,
    },
    /// A raw escape/CSI/OSC sequence captured verbatim (starts with ESC).
    Escape(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    /// ESC # intermediate — waiting for the final byte (e.g., '8' for DECALN).
    EscHash,
    /// ESC ( / ESC ) / ESC * / ESC + — waiting for charset designator byte.
    /// `slot`: 0 = G0, 1 = G1, 2 = G2, 3 = G3.
    EscCharset {
        slot: u8,
    },
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
        self.feed_into(bytes, &mut out);
        out
    }

    /// Feed a chunk of bytes, appending parsed actions to `out`.
    ///
    /// This avoids allocating a new Vec per call — callers on hot paths can
    /// reuse the same output buffer across frames by calling `out.clear()`
    /// before each invocation while retaining the underlying capacity.
    pub fn feed_into(&mut self, bytes: &[u8], out: &mut Vec<Action>) {
        for &b in bytes {
            if let Some(action) = self.advance(b) {
                out.push(action);
            }
        }
    }

    /// Advance the parser by one byte.
    ///
    /// Returns an action when a complete token is recognized.
    pub fn advance(&mut self, b: u8) -> Option<Action> {
        match self.state {
            State::Ground => self.advance_ground(b),
            State::Esc => self.advance_esc(b),
            State::EscHash => self.advance_esc_hash(b),
            State::EscCharset { slot } => self.advance_esc_charset(b, slot),
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
            // HTS: set tab stop at current column (ESC H)
            b'H' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::SetTabStop)
            }
            // DECKPAM: application keypad mode (ESC =)
            b'=' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::ApplicationKeypad)
            }
            // DECKPNM: normal keypad mode (ESC >)
            b'>' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::NormalKeypad)
            }
            // ESC # intermediate — wait for the final byte (e.g., DECALN).
            b'#' => {
                self.state = State::EscHash;
                None
            }
            // Character set designation: ESC ( / ESC ) / ESC * / ESC +
            b'(' => {
                self.state = State::EscCharset { slot: 0 };
                None
            }
            b')' => {
                self.state = State::EscCharset { slot: 1 };
                None
            }
            b'*' => {
                self.state = State::EscCharset { slot: 2 };
                None
            }
            b'+' => {
                self.state = State::EscCharset { slot: 3 };
                None
            }
            // SS2: single shift to G2 (ESC N)
            b'N' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::SingleShift2)
            }
            // SS3: single shift to G3 (ESC O)
            b'O' => {
                self.state = State::Ground;
                self.buf.clear();
                Some(Action::SingleShift3)
            }
            _ => {
                self.state = State::Ground;
                Some(Action::Escape(self.take_buf()))
            }
        }
    }

    fn advance_esc_hash(&mut self, b: u8) -> Option<Action> {
        self.buf.push(b);
        self.state = State::Ground;
        match b {
            // DECALN: DEC screen alignment test (ESC # 8).
            b'8' => {
                self.buf.clear();
                Some(Action::ScreenAlignment)
            }
            _ => Some(Action::Escape(self.take_buf())),
        }
    }

    fn advance_esc_charset(&mut self, b: u8, slot: u8) -> Option<Action> {
        self.buf.push(b);
        self.state = State::Ground;
        // The final byte is the charset designator (e.g., 'B' for ASCII, '0' for DEC Special).
        self.buf.clear();
        Some(Action::DesignateCharset { slot, charset: b })
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
        // Replace with a same-capacity Vec so `self.buf` retains its heap
        // allocation for the next escape sequence (avoids repeated growth
        // from zero).  The original swap-with-empty lost all capacity.
        let cap = self.buf.capacity();
        core::mem::replace(&mut self.buf, Vec::with_capacity(cap))
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

        // Check for `>` prefix (secondary device attributes).
        if param_bytes.first() == Some(&b'>') {
            return match final_byte {
                // DA2 (CSI > c / CSI > 0 c): secondary device attributes.
                b'c' => Some(Action::DeviceAttributesSecondary),
                _ => None,
            };
        }

        // Check for `<` prefix (SGR mouse mode 1006).
        // Format: CSI < Pb ; Px ; Py M (press) or CSI < Pb ; Px ; Py m (release).
        if param_bytes.first() == Some(&b'<') {
            if final_byte == b'M' || final_byte == b'm' {
                let params = Self::parse_csi_params(&param_bytes[1..])?;
                if params.len() == 3 {
                    let button = params[0];
                    // SGR mouse coords are 1-based; convert to 0-based.
                    let col = params[1].saturating_sub(1);
                    let row = params[2].saturating_sub(1);
                    let pressed = final_byte == b'M';
                    return Some(Action::MouseEvent {
                        button,
                        col,
                        row,
                        pressed,
                    });
                }
            }
            return None;
        }

        // Separate intermediate bytes (0x20..=0x2F per ECMA-48) from parameter
        // bytes. Intermediates follow parameters and precede the final byte.
        let intermediate_start = param_bytes
            .iter()
            .position(|&b| (0x20..=0x2F).contains(&b))
            .unwrap_or(param_bytes.len());
        let (numeric_bytes, intermediates) = param_bytes.split_at(intermediate_start);

        // Dispatch sequences with intermediate bytes first.
        match (intermediates, final_byte) {
            // DECSCUSR (CSI Ps SP q): set cursor shape.
            (b" ", b'q') => {
                let params = Self::parse_csi_params(numeric_bytes)?;
                let shape = params.first().copied().unwrap_or(0);
                return Some(Action::SetCursorShape(shape.min(6) as u8));
            }
            // DECSTR (CSI ! p): soft terminal reset.
            (b"!", b'p') => return Some(Action::SoftReset),
            // Unknown intermediate+final combination.
            _ if !intermediates.is_empty() => return None,
            _ => {}
        }

        // Standard CSI dispatch (no intermediate bytes).
        let params = Self::parse_csi_params(numeric_bytes)?;

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
                match mode {
                    0..=2 => Some(Action::EraseInDisplay(mode as u8)),
                    3 => Some(Action::EraseScrollback),
                    _ => None,
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
            // TBC: tab clear (CSI Ps g)
            b'g' => {
                let mode = params.first().copied().unwrap_or(0);
                Some(Action::ClearTabStop(mode))
            }
            // CBT: cursor backward tabulation (CSI Ps Z)
            b'Z' => Some(Action::BackTab(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            // ECH: erase characters at cursor (CSI Ps X)
            b'X' => Some(Action::EraseChars(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            // REP: repeat the most recently printed graphic character (CSI Ps b)
            b'b' => Some(Action::RepeatChar(Self::csi_count_or_one(
                params.first().copied(),
            ))),
            // SCOSC: save cursor position (ANSI-style, CSI s)
            b's' => {
                if params.is_empty() || (params.len() == 1 && params[0] == 0) {
                    Some(Action::SaveCursor)
                } else {
                    // CSI with params on 's' could be DECSLRM; ignore for now.
                    None
                }
            }
            // SCORC: restore cursor position (ANSI-style, CSI u)
            b'u' => {
                if params.is_empty() || (params.len() == 1 && params[0] == 0) {
                    Some(Action::RestoreCursor)
                } else {
                    None
                }
            }
            // Focus events (CSI I / CSI O)
            b'I' => Some(Action::FocusIn),
            b'O' => Some(Action::FocusOut),
            // Bracketed paste + function keys (CSI Ps ~)
            b'~' => {
                let param = params.first().copied().unwrap_or(0);
                match param {
                    200 => Some(Action::PasteStart),
                    201 => Some(Action::PasteEnd),
                    _ => None,
                }
            }
            // DA1: primary device attributes (CSI c / CSI 0 c)
            b'c' => {
                let p = params.first().copied().unwrap_or(0);
                if p == 0 {
                    Some(Action::DeviceAttributes)
                } else {
                    None
                }
            }
            // DSR / CPR: device status report (CSI Ps n)
            b'n' => {
                let p = params.first().copied().unwrap_or(0);
                match p {
                    5 => Some(Action::DeviceStatusReport),
                    6 => Some(Action::CursorPositionReport),
                    _ => None,
                }
            }
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

    fn parse_csi_params(params: &[u8]) -> Option<CsiParams> {
        if params.is_empty() {
            return Some(CsiParams::new());
        }
        let s = core::str::from_utf8(params).ok()?;
        let mut out = CsiParams::new();
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
    use smallvec::smallvec;

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
        assert_eq!(actions, vec![Action::DecRst(smallvec![25])]);
    }

    #[test]
    fn decset_cursor_show() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[?25h");
        assert_eq!(actions, vec![Action::DecSet(smallvec![25])]);
    }

    #[test]
    fn decset_multiple_modes() {
        let mut p = Parser::new();
        // Enable alt screen + bracketed paste + mouse SGR in one sequence
        let actions = p.feed(b"\x1b[?1049;2004;1006h");
        assert_eq!(actions, vec![Action::DecSet(smallvec![1049, 2004, 1006])]);
    }

    #[test]
    fn decrst_multiple_modes() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[?1049;2004l");
        assert_eq!(actions, vec![Action::DecRst(smallvec![1049, 2004])]);
    }

    #[test]
    fn decset_sync_output() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b[?2026h"),
            vec![Action::DecSet(smallvec![2026])]
        );
        assert_eq!(
            p.feed(b"\x1b[?2026l"),
            vec![Action::DecRst(smallvec![2026])]
        );
    }

    #[test]
    fn decset_autowrap() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[?7h"), vec![Action::DecSet(smallvec![7])]);
        assert_eq!(p.feed(b"\x1b[?7l"), vec![Action::DecRst(smallvec![7])]);
    }

    // ── ANSI SM / RM ──────────────────────────────────────────────

    #[test]
    fn ansi_set_insert_mode() {
        let mut p = Parser::new();
        // SM (CSI 4 h) — set insert mode
        assert_eq!(p.feed(b"\x1b[4h"), vec![Action::AnsiSet(smallvec![4])]);
        // RM (CSI 4 l) — reset insert mode
        assert_eq!(p.feed(b"\x1b[4l"), vec![Action::AnsiRst(smallvec![4])]);
    }

    #[test]
    fn ansi_set_newline_mode() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[20h"), vec![Action::AnsiSet(smallvec![20])]);
        assert_eq!(p.feed(b"\x1b[20l"), vec![Action::AnsiRst(smallvec![20])]);
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
        assert_eq!(p.feed(b"\x1b[31m"), vec![Action::Sgr(smallvec![31])]);
        assert_eq!(p.feed(b"\x1b[m"), vec![Action::Sgr(smallvec![])]);
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
                Action::Sgr(smallvec![31]),
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
                Action::DecSet(smallvec![1049]),
                Action::DecSet(smallvec![2004]),
                Action::DecSet(smallvec![1006]),
                Action::DecRst(smallvec![25]),
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
                Action::DecSet(smallvec![25]),
                Action::DecRst(smallvec![1006]),
                Action::DecRst(smallvec![2004]),
                Action::DecRst(smallvec![1049]),
            ]
        );
    }

    // ── HTS / TBC / CBT (tab stop management) ────────────────────

    #[test]
    fn esc_h_is_set_tab_stop() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bH"), vec![Action::SetTabStop]);
    }

    #[test]
    fn csi_g_is_clear_tab_stop_at_cursor() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[g"), vec![Action::ClearTabStop(0)]);
        assert_eq!(p.feed(b"\x1b[0g"), vec![Action::ClearTabStop(0)]);
    }

    #[test]
    fn csi_3g_is_clear_all_tab_stops() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[3g"), vec![Action::ClearTabStop(3)]);
    }

    #[test]
    fn csi_z_is_back_tab() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[Z"), vec![Action::BackTab(1)]);
        assert_eq!(p.feed(b"\x1b[3Z"), vec![Action::BackTab(3)]);
    }

    // ── DECKPAM / DECKPNM (keypad modes) ─────────────────────────

    #[test]
    fn esc_eq_is_application_keypad() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b="), vec![Action::ApplicationKeypad]);
    }

    #[test]
    fn esc_gt_is_normal_keypad() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b>"), vec![Action::NormalKeypad]);
    }

    // ── ECH (erase characters) ────────────────────────────────────

    #[test]
    fn csi_x_is_erase_chars() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[X"), vec![Action::EraseChars(1)]);
        assert_eq!(p.feed(b"\x1b[5X"), vec![Action::EraseChars(5)]);
    }

    // ── Mixed new sequences integration ───────────────────────────

    #[test]
    fn tab_stop_setup_and_clear_sequence() {
        let mut p = Parser::new();
        // Move to col 4, set tab, move to col 12, set tab, then clear all
        let actions = p.feed(b"\x1b[5G\x1bH\x1b[13G\x1bH\x1b[3g");
        assert_eq!(
            actions,
            vec![
                Action::CursorColumn(4),
                Action::SetTabStop,
                Action::CursorColumn(12),
                Action::SetTabStop,
                Action::ClearTabStop(3),
            ]
        );
    }

    #[test]
    fn esc_hash_8_is_screen_alignment() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b#8"), vec![Action::ScreenAlignment]);
    }

    #[test]
    fn esc_hash_unknown_is_escape() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b#3");
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Escape(_)));
    }

    #[test]
    fn csi_b_is_repeat_char() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[5b"), vec![Action::RepeatChar(5)]);
    }

    #[test]
    fn csi_b_default_is_one() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[b"), vec![Action::RepeatChar(1)]);
    }

    // ── DECSCUSR (cursor shape) ──────────────────────────────────

    #[test]
    fn csi_sp_q_is_set_cursor_shape() {
        let mut p = Parser::new();
        // CSI 2 SP q = steady block
        assert_eq!(p.feed(b"\x1b[2 q"), vec![Action::SetCursorShape(2)]);
    }

    #[test]
    fn csi_sp_q_all_shapes() {
        let mut p = Parser::new();
        // 0=default, 1=blinking block, 2=steady block, 3=blinking underline,
        // 4=steady underline, 5=blinking bar, 6=steady bar
        for shape in 0u8..=6 {
            let seq = format!("\x1b[{shape} q");
            let actions = p.feed(seq.as_bytes());
            assert_eq!(
                actions,
                vec![Action::SetCursorShape(shape)],
                "DECSCUSR shape {shape}"
            );
        }
    }

    #[test]
    fn csi_sp_q_default_is_zero() {
        let mut p = Parser::new();
        // CSI SP q (no parameter) = default cursor shape (0)
        assert_eq!(p.feed(b"\x1b[ q"), vec![Action::SetCursorShape(0)]);
    }

    #[test]
    fn csi_sp_q_clamps_to_six() {
        let mut p = Parser::new();
        // Values above 6 are clamped
        assert_eq!(p.feed(b"\x1b[99 q"), vec![Action::SetCursorShape(6)]);
    }

    // ── DECSTR (soft reset) ──────────────────────────────────────

    #[test]
    fn csi_bang_p_is_soft_reset() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[!p"), vec![Action::SoftReset]);
    }

    #[test]
    fn vim_cursor_shape_sequence() {
        let mut p = Parser::new();
        // Typical vim: set steady bar on insert, steady block on normal
        let actions = p.feed(b"\x1b[6 q\x1b[2 q");
        assert_eq!(
            actions,
            vec![
                Action::SetCursorShape(6), // steady bar (insert mode)
                Action::SetCursorShape(2), // steady block (normal mode)
            ]
        );
    }

    #[test]
    fn soft_reset_in_typical_sequence() {
        let mut p = Parser::new();
        // Soft reset, then set up modes
        let actions = p.feed(b"\x1b[!p\x1b[?7h\x1b[?25h");
        assert_eq!(
            actions,
            vec![
                Action::SoftReset,
                Action::DecSet(smallvec![7]),  // enable autowrap
                Action::DecSet(smallvec![25]), // show cursor
            ]
        );
    }

    // ── ED mode 3 (erase scrollback) ─────────────────────────────

    #[test]
    fn csi_3_j_is_erase_scrollback() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[3J"), vec![Action::EraseScrollback]);
    }

    #[test]
    fn csi_j_mode_0_1_2_still_work() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[J"), vec![Action::EraseInDisplay(0)]);
        assert_eq!(p.feed(b"\x1b[1J"), vec![Action::EraseInDisplay(1)]);
        assert_eq!(p.feed(b"\x1b[2J"), vec![Action::EraseInDisplay(2)]);
    }

    // ── SCOSC / SCORC (CSI s / CSI u) ───────────────────────────

    #[test]
    fn csi_s_is_save_cursor() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[s"), vec![Action::SaveCursor]);
    }

    #[test]
    fn csi_u_is_restore_cursor() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[u"), vec![Action::RestoreCursor]);
    }

    #[test]
    fn csi_s_with_params_is_none() {
        let mut p = Parser::new();
        // CSI 1;2 s could be DECSLRM — should not emit SaveCursor.
        // Unrecognised CSI sequences fall through as Action::Escape(raw).
        let result = p.feed(b"\x1b[1;2s");
        assert_eq!(result, vec![Action::Escape(b"\x1b[1;2s".to_vec())]);
    }

    // ── Focus events (CSI I / CSI O) ─────────────────────────────

    #[test]
    fn csi_i_is_focus_in() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[I"), vec![Action::FocusIn]);
    }

    #[test]
    fn csi_o_is_focus_out() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[O"), vec![Action::FocusOut]);
    }

    // ── Bracketed paste (CSI 200~/201~) ──────────────────────────

    #[test]
    fn csi_200_tilde_is_paste_start() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[200~"), vec![Action::PasteStart]);
    }

    #[test]
    fn csi_201_tilde_is_paste_end() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[201~"), vec![Action::PasteEnd]);
    }

    #[test]
    fn csi_unknown_tilde_is_none() {
        let mut p = Parser::new();
        // CSI 99 ~ is an unknown function-key — not dispatched, falls through
        // as Action::Escape(raw).
        let result = p.feed(b"\x1b[99~");
        assert_eq!(result, vec![Action::Escape(b"\x1b[99~".to_vec())]);
    }

    // ── DA1: primary device attributes (CSI c) ──────────────────

    #[test]
    fn csi_c_is_device_attributes() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[c"), vec![Action::DeviceAttributes]);
    }

    #[test]
    fn csi_0_c_is_device_attributes() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[0c"), vec![Action::DeviceAttributes]);
    }

    #[test]
    fn csi_1_c_is_not_device_attributes() {
        let mut p = Parser::new();
        // CSI 1 c has a non-zero param — not DA1.
        let actions = p.feed(b"\x1b[1c");
        assert!(!actions.contains(&Action::DeviceAttributes));
    }

    // ── DA2: secondary device attributes (CSI > c) ──────────────

    #[test]
    fn csi_gt_c_is_da2() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[>c"), vec![Action::DeviceAttributesSecondary]);
    }

    #[test]
    fn csi_gt_0_c_is_da2() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[>0c"), vec![Action::DeviceAttributesSecondary]);
    }

    // ── DSR / CPR (CSI n) ────────────────────────────────────────

    #[test]
    fn csi_5n_is_device_status_report() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[5n"), vec![Action::DeviceStatusReport]);
    }

    #[test]
    fn csi_6n_is_cursor_position_report() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[6n"), vec![Action::CursorPositionReport]);
    }

    #[test]
    fn csi_0n_is_not_dsr() {
        let mut p = Parser::new();
        let actions = p.feed(b"\x1b[0n");
        assert!(!actions.contains(&Action::DeviceStatusReport));
        assert!(!actions.contains(&Action::CursorPositionReport));
    }

    // ── Character set designation (ESC ( / ESC )) ────────────────

    #[test]
    fn esc_paren_b_is_g0_ascii() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b(B"),
            vec![Action::DesignateCharset {
                slot: 0,
                charset: b'B'
            }]
        );
    }

    #[test]
    fn esc_paren_0_is_g0_dec_graphics() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b(0"),
            vec![Action::DesignateCharset {
                slot: 0,
                charset: b'0'
            }]
        );
    }

    #[test]
    fn esc_rparen_b_is_g1_ascii() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b)B"),
            vec![Action::DesignateCharset {
                slot: 1,
                charset: b'B'
            }]
        );
    }

    #[test]
    fn esc_star_0_is_g2_dec_graphics() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b*0"),
            vec![Action::DesignateCharset {
                slot: 2,
                charset: b'0'
            }]
        );
    }

    #[test]
    fn esc_plus_b_is_g3_ascii() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b+B"),
            vec![Action::DesignateCharset {
                slot: 3,
                charset: b'B'
            }]
        );
    }

    // ── Single Shift (ESC N / ESC O) ─────────────────────────────

    #[test]
    fn esc_n_is_single_shift_2() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bN"), vec![Action::SingleShift2]);
    }

    #[test]
    fn esc_o_is_single_shift_3() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1bO"), vec![Action::SingleShift3]);
    }

    // ── SGR Mouse (CSI < Pb ; Px ; Py M/m) ──────────────────────

    #[test]
    fn sgr_mouse_left_press() {
        let mut p = Parser::new();
        // CSI < 0 ; 10 ; 5 M → left button press at col 9, row 4 (0-based)
        assert_eq!(
            p.feed(b"\x1b[<0;10;5M"),
            vec![Action::MouseEvent {
                button: 0,
                col: 9,
                row: 4,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_left_release() {
        let mut p = Parser::new();
        // CSI < 0 ; 10 ; 5 m → left button release at col 9, row 4
        assert_eq!(
            p.feed(b"\x1b[<0;10;5m"),
            vec![Action::MouseEvent {
                button: 0,
                col: 9,
                row: 4,
                pressed: false,
            }]
        );
    }

    #[test]
    fn sgr_mouse_right_press() {
        let mut p = Parser::new();
        // CSI < 2 ; 1 ; 1 M → right button press at col 0, row 0
        assert_eq!(
            p.feed(b"\x1b[<2;1;1M"),
            vec![Action::MouseEvent {
                button: 2,
                col: 0,
                row: 0,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_middle_press() {
        let mut p = Parser::new();
        assert_eq!(
            p.feed(b"\x1b[<1;50;25M"),
            vec![Action::MouseEvent {
                button: 1,
                col: 49,
                row: 24,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_scroll_up() {
        let mut p = Parser::new();
        // button 64 = scroll up
        assert_eq!(
            p.feed(b"\x1b[<64;5;3M"),
            vec![Action::MouseEvent {
                button: 64,
                col: 4,
                row: 2,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_scroll_down() {
        let mut p = Parser::new();
        // button 65 = scroll down
        assert_eq!(
            p.feed(b"\x1b[<65;5;3M"),
            vec![Action::MouseEvent {
                button: 65,
                col: 4,
                row: 2,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_ctrl_left_press() {
        let mut p = Parser::new();
        // button 16 = ctrl modifier + left button (0 + 16)
        assert_eq!(
            p.feed(b"\x1b[<16;1;1M"),
            vec![Action::MouseEvent {
                button: 16,
                col: 0,
                row: 0,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_motion_while_pressed() {
        let mut p = Parser::new();
        // button 32 = motion with left button held (0 + 32)
        assert_eq!(
            p.feed(b"\x1b[<32;20;10M"),
            vec![Action::MouseEvent {
                button: 32,
                col: 19,
                row: 9,
                pressed: true,
            }]
        );
    }

    #[test]
    fn sgr_mouse_incomplete_params_falls_through() {
        let mut p = Parser::new();
        // Only 2 params instead of 3
        let actions = p.feed(b"\x1b[<0;10M");
        assert!(
            !matches!(actions.first(), Some(Action::MouseEvent { .. })),
            "incomplete SGR mouse should not produce MouseEvent"
        );
    }

    #[test]
    fn sgr_mouse_large_coords() {
        let mut p = Parser::new();
        // Large coordinates (wide terminal)
        assert_eq!(
            p.feed(b"\x1b[<0;300;100M"),
            vec![Action::MouseEvent {
                button: 0,
                col: 299,
                row: 99,
                pressed: true,
            }]
        );
    }

    // ── feed_into equivalence ──────────────────────────────────────

    #[test]
    fn feed_into_is_equivalent_to_feed() {
        let inputs: &[&[u8]] = &[
            b"Hello, World!",
            b"\x1b[31;1mBold Red\x1b[m",
            b"\x1b[?1049h\x1b[?2004h",
            b"\x1b[10;20H",
            b"\x1b]0;title\x07",
            b"\xc3\xa9\xc3\xa0",
        ];
        for &input in inputs {
            let mut p1 = Parser::new();
            let mut p2 = Parser::new();
            let expected = p1.feed(input);
            let mut actual = Vec::new();
            p2.feed_into(input, &mut actual);
            assert_eq!(expected, actual, "mismatch for input {:?}", input);
        }
    }

    #[test]
    fn feed_into_reuses_capacity() {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed_into(b"\x1b[31m", &mut out);
        assert!(!out.is_empty());
        let cap = out.capacity();
        out.clear();
        p.feed_into(b"\x1b[32m", &mut out);
        // Capacity should be retained from the previous call.
        assert!(out.capacity() >= cap);
    }
}
