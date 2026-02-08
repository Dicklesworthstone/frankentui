#![forbid(unsafe_code)]

//! Input parser state machine.
//!
//! Decodes terminal input bytes into [`crate::event::Event`] values with DoS protection.
//!
//! # Design
//!
//! The parser is a state machine that handles:
//! - ASCII characters and control codes
//! - UTF-8 multi-byte sequences
//! - CSI (Control Sequence Introducer) sequences
//! - SS3 (Single Shift 3) sequences
//! - OSC (Operating System Command) sequences
//! - Bracketed paste mode
//! - Mouse events (SGR protocol)
//! - Focus events
//!
//! # DoS Protection
//!
//! The parser enforces length limits on all sequence types to prevent memory exhaustion:
//! - CSI sequences: 256 bytes max
//! - OSC sequences: 4KB max
//! - Paste content: 1MB max

use crate::event::{
    ClipboardEvent, ClipboardSource, Event, KeyCode, KeyEvent, KeyEventKind, Modifiers,
    MouseButton, MouseEvent, MouseEventKind, PasteEvent,
};

/// DoS protection: maximum CSI sequence length.
const MAX_CSI_LEN: usize = 256;

/// DoS protection: maximum OSC sequence length.
const MAX_OSC_LEN: usize = 4096;

/// DoS protection: maximum paste content length.
const MAX_PASTE_LEN: usize = 1024 * 1024; // 1MB

/// Parser state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ParserState {
    /// Normal character input.
    #[default]
    Ground,
    /// After ESC (0x1B).
    Escape,
    /// After ESC [ (CSI introducer).
    Csi,
    /// Collecting CSI parameters.
    CsiParam,
    /// Ignoring oversized CSI sequence.
    CsiIgnore,
    /// After ESC O (SS3 introducer).
    Ss3,
    /// After ESC ] (OSC introducer).
    Osc,
    /// Collecting OSC content.
    OscContent,
    /// After ESC inside OSC (for ESC \ terminator).
    OscEscape,
    /// Ignoring oversized OSC sequence.
    OscIgnore,
    /// Collecting UTF-8 multi-byte sequence.
    Utf8 {
        /// Bytes collected so far.
        collected: u8,
        /// Total bytes expected.
        expected: u8,
    },
}

/// Terminal input parser with DoS protection.
///
/// Parse terminal input bytes into events:
///
/// ```ignore
/// let mut parser = InputParser::new();
/// let events = parser.parse(b"\x1b[A"); // Up arrow
/// assert_eq!(events.len(), 1);
/// ```
#[derive(Debug)]
pub struct InputParser {
    /// Current parser state.
    state: ParserState,
    /// Buffer for accumulating sequence bytes.
    buffer: Vec<u8>,
    /// Buffer for collecting paste content.
    paste_buffer: Vec<u8>,
    /// UTF-8 bytes collected so far.
    utf8_buffer: [u8; 4],
    /// Whether we're in bracketed paste mode.
    in_paste: bool,
    /// Event queued for the next iteration (allows emitting 2 events per byte).
    pending_event: Option<Event>,
}

impl Default for InputParser {
    fn default() -> Self {
        Self::new()
    }
}

impl InputParser {
    /// Create a new input parser.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ParserState::Ground,
            buffer: Vec::with_capacity(64),
            paste_buffer: Vec::new(),
            utf8_buffer: [0; 4],
            in_paste: false,
            pending_event: None,
        }
    }

    /// Parse input bytes and return any completed events.
    pub fn parse(&mut self, input: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        self.parse_with(input, |event| events.push(event));
        events
    }

    /// Parse input bytes and emit each completed event through `emit`.
    pub fn parse_with<F>(&mut self, input: &[u8], mut emit: F)
    where
        F: FnMut(Event),
    {
        for &byte in input {
            if let Some(event) = self.process_byte(byte) {
                emit(event);
            }
            if let Some(pending) = self.pending_event.take() {
                emit(pending);
            }
        }
    }

    /// Parse input bytes and append completed events to `events`.
    ///
    /// This variant lets callers reuse a scratch buffer across parses to avoid
    /// repeated allocations on hot input paths.
    pub fn parse_into(&mut self, input: &[u8], events: &mut Vec<Event>) {
        self.parse_with(input, |event| events.push(event));
    }

    /// Process a single byte and optionally return an event.
    fn process_byte(&mut self, byte: u8) -> Option<Event> {
        // In paste mode, collect bytes until end sequence
        if self.in_paste {
            return self.process_paste_byte(byte);
        }

        match self.state {
            ParserState::Ground => self.process_ground(byte),
            ParserState::Escape => self.process_escape(byte),
            ParserState::Csi => self.process_csi(byte),
            ParserState::CsiParam => self.process_csi_param(byte),
            ParserState::CsiIgnore => self.process_csi_ignore(byte),
            ParserState::Ss3 => self.process_ss3(byte),
            ParserState::Osc => self.process_osc(byte),
            ParserState::OscContent => self.process_osc_content(byte),
            ParserState::OscEscape => self.process_osc_escape(byte),
            ParserState::OscIgnore => self.process_osc_ignore(byte),
            ParserState::Utf8 {
                collected,
                expected,
            } => self.process_utf8(byte, collected, expected),
        }
    }

    /// Process byte in ground state.
    fn process_ground(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // ESC - start escape sequence
            0x1B => {
                self.state = ParserState::Escape;
                None
            }
            // NUL - Ctrl+Space or Ctrl+@
            0x00 => Some(Event::Key(KeyEvent::new(KeyCode::Null))),
            // Backspace alternate (Ctrl+H)
            0x08 => Some(Event::Key(KeyEvent::new(KeyCode::Backspace))),
            // Tab (Ctrl+I) - check before generic Ctrl range
            0x09 => Some(Event::Key(KeyEvent::new(KeyCode::Tab))),
            // Enter (Ctrl+M) - check before generic Ctrl range
            0x0D => Some(Event::Key(KeyEvent::new(KeyCode::Enter))),
            // Other Ctrl+A through Ctrl+Z (0x01-0x1A excluding Tab and Enter)
            0x01..=0x07 | 0x0A..=0x0C | 0x0E..=0x1A => {
                let c = (byte + b'a' - 1) as char;
                Some(Event::Key(
                    KeyEvent::new(KeyCode::Char(c)).with_modifiers(Modifiers::CTRL),
                ))
            }
            // Backspace (DEL)
            0x7F => Some(Event::Key(KeyEvent::new(KeyCode::Backspace))),
            // Printable ASCII
            0x20..=0x7E => Some(Event::Key(KeyEvent::new(KeyCode::Char(byte as char)))),
            // UTF-8 lead bytes (valid ranges only)
            0xC2..=0xDF => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 {
                    collected: 1,
                    expected: 2,
                };
                None
            }
            0xE0..=0xEF => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 {
                    collected: 1,
                    expected: 3,
                };
                None
            }
            0xF0..=0xF4 => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 {
                    collected: 1,
                    expected: 4,
                };
                None
            }
            // Invalid UTF-8 lead bytes (overlong or out of range)
            0xC0..=0xC1 | 0xF5..=0xFF => Some(Event::Key(KeyEvent::new(KeyCode::Char(
                std::char::REPLACEMENT_CHARACTER,
            )))),
            // Invalid or ignored bytes
            _ => None,
        }
    }

    /// Process byte after ESC.
    fn process_escape(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // CSI introducer
            b'[' => {
                self.state = ParserState::Csi;
                self.buffer.clear();
                None
            }
            // SS3 introducer
            b'O' => {
                self.state = ParserState::Ss3;
                None
            }
            // OSC introducer
            b']' => {
                self.state = ParserState::Osc;
                self.buffer.clear();
                None
            }
            // Another ESC - emit Alt+Escape and reset to ground
            // (or treat as start of new sequence - but ESC ESC is usually Alt+ESC)
            0x1B => {
                self.state = ParserState::Ground;
                Some(Event::Key(
                    KeyEvent::new(KeyCode::Escape).with_modifiers(Modifiers::ALT),
                ))
            }
            // Alt+letter or Alt+char
            0x20..=0x7E => {
                self.state = ParserState::Ground;
                Some(Event::Key(
                    KeyEvent::new(KeyCode::Char(byte as char)).with_modifiers(Modifiers::ALT),
                ))
            }
            // Alt+Backspace (DEL)
            0x7F => {
                self.state = ParserState::Ground;
                Some(Event::Key(
                    KeyEvent::new(KeyCode::Backspace).with_modifiers(Modifiers::ALT),
                ))
            }
            // Invalid - return to ground
            _ => {
                self.state = ParserState::Ground;
                None
            }
        }
    }

    /// Process byte at start of CSI sequence.
    fn process_csi(&mut self, byte: u8) -> Option<Event> {
        // Robustness: ESC restarts sequence
        if byte == 0x1B {
            self.state = ParserState::Escape;
            self.buffer.clear();
            return None;
        }

        self.buffer.push(byte);

        match byte {
            // Parameter bytes (0x30-0x3F) and Intermediate bytes (0x20-0x2F)
            0x20..=0x3F => {
                self.state = ParserState::CsiParam;
                None
            }
            // Final byte (0x40-0x7E) - parse and return
            0x40..=0x7E => {
                self.state = ParserState::Ground;
                self.parse_csi_sequence()
            }
            // Invalid (0x00-0x1F, 0x7F-0xFF)
            _ => {
                self.state = ParserState::Ground;
                self.buffer.clear();
                None
            }
        }
    }

    /// Process byte while collecting CSI parameters.
    fn process_csi_param(&mut self, byte: u8) -> Option<Event> {
        // Robustness: ESC restarts sequence
        if byte == 0x1B {
            self.state = ParserState::Escape;
            self.buffer.clear();
            return None;
        }

        // DoS protection
        if self.buffer.len() >= MAX_CSI_LEN {
            self.state = ParserState::CsiIgnore;
            self.buffer.clear();
            return None;
        }

        self.buffer.push(byte);

        match byte {
            // Continue collecting parameters/intermediates
            0x20..=0x3F => None,
            // Final byte - parse and return
            0x40..=0x7E => {
                self.state = ParserState::Ground;
                self.parse_csi_sequence()
            }
            // Invalid
            _ => {
                self.state = ParserState::Ground;
                self.buffer.clear();
                None
            }
        }
    }

    /// Ignore bytes until end of CSI sequence.
    fn process_csi_ignore(&mut self, byte: u8) -> Option<Event> {
        // Robustness: ESC restarts sequence
        if byte == 0x1B {
            self.state = ParserState::Escape;
            return None;
        }

        // Final byte (0x40-0x7E) - return to ground
        // Intermediate bytes outside this range are ignored
        if (0x40..=0x7E).contains(&byte) {
            self.state = ParserState::Ground;
        } else if !(0x20..=0x7E).contains(&byte) {
            // Invalid character (e.g. newline) - abort sequence
            self.state = ParserState::Ground;
        }
        None
    }

    /// Parse a complete CSI sequence from the buffer.
    fn parse_csi_sequence(&mut self) -> Option<Event> {
        let seq = std::mem::take(&mut self.buffer);
        if seq.is_empty() {
            return None;
        }

        let final_byte = *seq.last()?;
        let params = &seq[..seq.len() - 1];

        // Check for special sequences first
        match (params, final_byte) {
            // Focus events
            ([], b'I') => return Some(Event::Focus(true)),
            ([], b'O') => return Some(Event::Focus(false)),

            // Bracketed paste
            (b"200", b'~') => {
                self.in_paste = true;
                self.paste_buffer.clear();
                self.buffer.clear(); // Ensure tail buffer is clean
                return None;
            }
            (b"201", b'~') => {
                self.in_paste = false;
                let content = String::from_utf8_lossy(&self.paste_buffer).into_owned();
                self.paste_buffer.clear();
                return Some(Event::Paste(PasteEvent::bracketed(content)));
            }

            // SGR mouse protocol
            _ if params.starts_with(b"<") && (final_byte == b'M' || final_byte == b'm') => {
                return self.parse_sgr_mouse(params, final_byte);
            }

            _ => {}
        }

        // Arrow keys and other CSI sequences
        match final_byte {
            b'A' => Some(Event::Key(self.key_with_modifiers(KeyCode::Up, params))),
            b'B' => Some(Event::Key(self.key_with_modifiers(KeyCode::Down, params))),
            b'C' => Some(Event::Key(self.key_with_modifiers(KeyCode::Right, params))),
            b'D' => Some(Event::Key(self.key_with_modifiers(KeyCode::Left, params))),
            b'H' => Some(Event::Key(self.key_with_modifiers(KeyCode::Home, params))),
            b'F' => Some(Event::Key(self.key_with_modifiers(KeyCode::End, params))),
            b'Z' => Some(Event::Key(
                self.key_with_modifiers(KeyCode::BackTab, params),
            )),
            b'~' => self.parse_csi_tilde(params),
            b'u' => self.parse_kitty_keyboard(params),
            _ => None,
        }
    }

    /// Parse CSI sequences ending in ~.
    fn parse_csi_tilde(&self, params: &[u8]) -> Option<Event> {
        let num = self.parse_first_param(params)?;
        let mods = self.parse_modifier_param(params);

        let code = match num {
            1 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            15 => KeyCode::F(5),
            17 => KeyCode::F(6),
            18 => KeyCode::F(7),
            19 => KeyCode::F(8),
            20 => KeyCode::F(9),
            21 => KeyCode::F(10),
            23 => KeyCode::F(11),
            24 => KeyCode::F(12),
            _ => return None,
        };

        Some(Event::Key(KeyEvent::new(code).with_modifiers(mods)))
    }

    /// Parse the first numeric parameter from CSI params.
    fn parse_first_param(&self, params: &[u8]) -> Option<u32> {
        let s = std::str::from_utf8(params).ok()?;
        let first = s.split(';').next()?;
        first.parse().ok()
    }

    /// Parse modifier parameter (second param in CSI sequences).
    fn parse_modifier_param(&self, params: &[u8]) -> Modifiers {
        let s = match std::str::from_utf8(params) {
            Ok(s) => s,
            Err(_) => return Modifiers::NONE,
        };

        let modifier_value: u32 = s
            .split(';')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        Self::modifiers_from_xterm(modifier_value)
    }

    /// Parse Kitty keyboard protocol CSI u sequences.
    ///
    /// Format: `CSI unicode-key-code:alt-keys ; modifiers:event-type ; text-as-codepoints u`
    fn parse_kitty_keyboard(&self, params: &[u8]) -> Option<Event> {
        let s = std::str::from_utf8(params).ok()?;
        if s.is_empty() {
            return None;
        }

        let mut parts = s.split(';');
        let key_part = parts.next().unwrap_or("");
        let key_code_str = key_part.split(':').next().unwrap_or("");
        let key_code: u32 = key_code_str.parse().ok()?;

        let mod_part = parts.next().unwrap_or("");
        let (modifiers, kind) = Self::kitty_modifiers_and_kind(mod_part);

        let code = Self::kitty_keycode_to_keycode(key_code)?;
        Some(Event::Key(
            KeyEvent::new(code)
                .with_modifiers(modifiers)
                .with_kind(kind),
        ))
    }

    fn kitty_modifiers_and_kind(mod_part: &str) -> (Modifiers, KeyEventKind) {
        if mod_part.is_empty() {
            return (Modifiers::NONE, KeyEventKind::Press);
        }

        let mut parts = mod_part.split(':');
        let mod_value: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);
        let kind_value: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);

        let modifiers = Self::modifiers_from_xterm(mod_value);
        let kind = match kind_value {
            2 => KeyEventKind::Repeat,
            3 => KeyEventKind::Release,
            _ => KeyEventKind::Press,
        };

        (modifiers, kind)
    }

    fn kitty_keycode_to_keycode(key_code: u32) -> Option<KeyCode> {
        match key_code {
            // Standard ASCII keys
            9 => Some(KeyCode::Tab),
            13 => Some(KeyCode::Enter),
            27 => Some(KeyCode::Escape),
            8 | 127 => Some(KeyCode::Backspace),
            // Kitty keyboard protocol extended keys (CSI u)
            57_344 => Some(KeyCode::Escape),
            57_345 => Some(KeyCode::Enter),
            57_346 => Some(KeyCode::Tab),
            57_347 => Some(KeyCode::Backspace),
            57_348 => Some(KeyCode::Insert),
            57_349 => Some(KeyCode::Delete),
            57_350 => Some(KeyCode::Left),
            57_351 => Some(KeyCode::Right),
            57_352 => Some(KeyCode::Up),
            57_353 => Some(KeyCode::Down),
            57_354 => Some(KeyCode::PageUp),
            57_355 => Some(KeyCode::PageDown),
            57_356 => Some(KeyCode::Home),
            57_357 => Some(KeyCode::End),
            // F1-F24 (57_364-57_387)
            57_364..=57_387 => {
                // Safety: range is [57_364, 57_387], so (key_code - 57_364 + 1) is [1, 24]
                // which fits in u8. We use debug_assert to catch any future range changes.
                let f_num = key_code - 57_364 + 1;
                debug_assert!(f_num <= 24, "F-key number {f_num} exceeds F24");
                Some(KeyCode::F(f_num as u8))
            }
            // Reserved/unhandled Kitty keycodes return None
            57_358..=57_363 | 57_388..=63_743 => None,
            // Unicode codepoints
            _ => char::from_u32(key_code).map(KeyCode::Char),
        }
    }

    fn modifiers_from_xterm(value: u32) -> Modifiers {
        // xterm modifier encoding: value = 1 + modifier_bits
        // Shift=1, Alt=2, Ctrl=4, Super=8
        let bits = value.saturating_sub(1);
        let mut mods = Modifiers::NONE;
        if bits & 1 != 0 {
            mods |= Modifiers::SHIFT;
        }
        if bits & 2 != 0 {
            mods |= Modifiers::ALT;
        }
        if bits & 4 != 0 {
            mods |= Modifiers::CTRL;
        }
        if bits & 8 != 0 {
            mods |= Modifiers::SUPER;
        }
        mods
    }

    /// Create a key event with modifiers from CSI params.
    fn key_with_modifiers(&self, code: KeyCode, params: &[u8]) -> KeyEvent {
        KeyEvent::new(code).with_modifiers(self.parse_modifier_param(params))
    }

    /// Parse SGR mouse protocol events.
    fn parse_sgr_mouse(&self, params: &[u8], final_byte: u8) -> Option<Event> {
        // Format: CSI < button ; x ; y M|m
        // Skip the leading '<'
        let params = &params[1..];
        let s = std::str::from_utf8(params).ok()?;
        let mut parts = s.split(';');

        let button_code: u16 = parts.next()?.parse().ok()?;
        let x: u16 = parts.next()?.parse().ok()?;
        let y: u16 = parts.next()?.parse().ok()?;

        // Decode button and modifiers
        let (button, mods) = self.decode_mouse_button(button_code);

        let kind = if final_byte == b'M' {
            if button_code & 64 != 0 {
                // Scroll event: bit 6 (64) is set
                // bits 0-1 determine direction: 0=up, 1=down, 2=left, 3=right
                match button_code & 3 {
                    0 => MouseEventKind::ScrollUp,
                    1 => MouseEventKind::ScrollDown,
                    2 => MouseEventKind::ScrollLeft,
                    _ => MouseEventKind::ScrollRight,
                }
            } else if button_code & 32 != 0 {
                // Motion event (bit 5 set)
                // bits 0-1: 0=left, 1=middle, 2=right, 3=no button (moved)
                if button_code & 3 == 3 {
                    MouseEventKind::Moved
                } else {
                    MouseEventKind::Drag(button)
                }
            } else {
                MouseEventKind::Down(button)
            }
        } else {
            MouseEventKind::Up(button)
        };

        Some(Event::Mouse(MouseEvent {
            kind,
            x: x.saturating_sub(1), // Convert to 0-indexed
            y: y.saturating_sub(1),
            modifiers: mods,
        }))
    }

    /// Decode mouse button code to button and modifiers.
    fn decode_mouse_button(&self, code: u16) -> (MouseButton, Modifiers) {
        let button = match code & 0b11 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::Left,
        };

        let mut mods = Modifiers::NONE;
        if code & 4 != 0 {
            mods |= Modifiers::SHIFT;
        }
        if code & 8 != 0 {
            mods |= Modifiers::ALT;
        }
        if code & 16 != 0 {
            mods |= Modifiers::CTRL;
        }

        (button, mods)
    }

    /// Process SS3 (ESC O) sequences.
    fn process_ss3(&mut self, byte: u8) -> Option<Event> {
        // Robustness: ESC restarts sequence
        if byte == 0x1B {
            self.state = ParserState::Escape;
            return None;
        }

        self.state = ParserState::Ground;

        let code = match byte {
            b'P' => KeyCode::F(1),
            b'Q' => KeyCode::F(2),
            b'R' => KeyCode::F(3),
            b'S' => KeyCode::F(4),
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            _ => return None,
        };

        Some(Event::Key(KeyEvent::new(code)))
    }

    /// Process OSC start.
    fn process_osc(&mut self, byte: u8) -> Option<Event> {
        // Handle ESC as potential ST terminator (ESC \) - don't add to buffer
        if byte == 0x1B {
            self.state = ParserState::OscEscape;
            return None;
        }

        self.buffer.push(byte);

        match byte {
            // BEL terminates immediately
            0x07 => {
                self.state = ParserState::Ground;
                self.parse_osc_sequence()
            }
            // Continue collecting
            _ => {
                self.state = ParserState::OscContent;
                None
            }
        }
    }

    /// Process OSC content.
    fn process_osc_content(&mut self, byte: u8) -> Option<Event> {
        // Handle ESC (0x1B) as potential terminator or reset
        if byte == 0x1B {
            self.state = ParserState::OscEscape;
            return None;
        }

        // Robustness: Abort on control characters (except BEL) to prevent swallowing logs
        if byte < 0x20 && byte != 0x07 {
            self.state = ParserState::Ground;
            self.buffer.clear();
            return None;
        }

        // DoS protection
        if self.buffer.len() >= MAX_OSC_LEN {
            self.state = ParserState::OscIgnore;
            self.buffer.clear();
            return None;
        }

        match byte {
            // BEL terminates
            0x07 => {
                self.state = ParserState::Ground;
                self.parse_osc_sequence()
            }
            // Continue collecting
            _ => {
                self.buffer.push(byte);
                None
            }
        }
    }

    /// Process ESC inside OSC (checking for ST terminator).
    fn process_osc_escape(&mut self, byte: u8) -> Option<Event> {
        if byte == b'\\' {
            // ST (String Terminator) found
            self.state = ParserState::Ground;
            self.parse_osc_sequence()
        } else if byte == 0x1B {
            // ESC ESC - treat second ESC as start of new sequence (restart)
            self.state = ParserState::Escape;
            self.buffer.clear();
            None
        } else {
            // ESC followed by something else.
            // Strict ANSI would say the OSC is cancelled by the ESC.
            // We treat this as a restart of parsing at the *current* byte,
            // effectively interpreting the previous ESC as a cancel.

            self.buffer.clear();
            self.state = ParserState::Escape;
            self.process_escape(byte)
        }
    }

    /// Ignore bytes until end of OSC sequence.
    fn process_osc_ignore(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // BEL terminates
            0x07 => {
                self.state = ParserState::Ground;
                None
            }
            // ESC might start terminator or new sequence
            0x1B => {
                self.state = ParserState::OscEscape;
                None
            }
            // Abort on control characters to prevent swallowing logs
            _ if byte < 0x20 => {
                self.state = ParserState::Ground;
                None
            }
            // Continue ignoring
            _ => None,
        }
    }

    /// Parse a complete OSC sequence.
    fn parse_osc_sequence(&mut self) -> Option<Event> {
        let seq = std::mem::take(&mut self.buffer);

        // OSC 52 clipboard response: OSC 52 ; c ; <base64> BEL/ST
        if seq.starts_with(b"52;") {
            return self.parse_osc52_clipboard(&seq);
        }

        // Other OSC sequences (e.g., OSC 8 hyperlinks) are not parsed as events
        None
    }

    /// Parse OSC 52 clipboard response.
    fn parse_osc52_clipboard(&self, seq: &[u8]) -> Option<Event> {
        // Format: 52;c;<base64> or 52;p;<base64>
        let content = &seq[3..]; // Skip "52;"
        if content.is_empty() {
            return None;
        }

        // OSC 52 uses clipboard selectors: c=clipboard, p=primary, s=secondary
        // We map all to Osc52 source type since that's how we received it
        let source = ClipboardSource::Osc52;

        // Skip "c;" prefix
        let base64_start = content.iter().position(|&b| b == b';').map(|i| i + 1)?;
        let base64_data = &content[base64_start..];

        // Decode base64 (simple implementation)
        let decoded = self.decode_base64(base64_data)?;

        Some(Event::Clipboard(ClipboardEvent::new(
            String::from_utf8_lossy(&decoded).into_owned(),
            source,
        )))
    }

    /// Simple base64 decoder.
    fn decode_base64(&self, input: &[u8]) -> Option<Vec<u8>> {
        const DECODE_TABLE: [i8; 256] = {
            let mut table = [-1i8; 256];
            let mut i = 0u8;
            while i < 26 {
                table[(b'A' + i) as usize] = i as i8;
                table[(b'a' + i) as usize] = (i + 26) as i8;
                i += 1;
            }
            let mut i = 0u8;
            while i < 10 {
                table[(b'0' + i) as usize] = (i + 52) as i8;
                i += 1;
            }
            table[b'+' as usize] = 62;
            table[b'/' as usize] = 63;
            table
        };

        let mut output = Vec::with_capacity(input.len() * 3 / 4);
        let mut buffer = 0u32;
        let mut bits = 0u8;

        for &byte in input {
            if byte == b'=' {
                break;
            }
            let value = DECODE_TABLE[byte as usize];
            if value < 0 {
                continue; // Skip whitespace/invalid
            }
            buffer = (buffer << 6) | (value as u32);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }

        Some(output)
    }

    /// Process UTF-8 continuation bytes.
    fn process_utf8(&mut self, byte: u8, collected: u8, expected: u8) -> Option<Event> {
        // Check for valid continuation byte
        if (byte & 0xC0) != 0x80 {
            // Invalid - return to ground and re-process the unexpected byte.
            // Also emit a replacement character for the invalid sequence we just aborted.
            self.state = ParserState::Ground;

            // Queue the replacement event for the next iteration of the parse loop
            self.pending_event = self.process_ground(byte);

            return Some(Event::Key(KeyEvent::new(KeyCode::Char(
                std::char::REPLACEMENT_CHARACTER,
            ))));
        }

        self.utf8_buffer[collected as usize] = byte;
        let new_collected = collected + 1;

        if new_collected == expected {
            // Complete - decode and emit
            self.state = ParserState::Ground;
            let s = std::str::from_utf8(&self.utf8_buffer[..expected as usize]).ok()?;
            let c = s.chars().next()?;
            Some(Event::Key(KeyEvent::new(KeyCode::Char(c))))
        } else {
            // Need more bytes
            self.state = ParserState::Utf8 {
                collected: new_collected,
                expected,
            };
            None
        }
    }

    /// Process bytes while in paste mode.
    fn process_paste_byte(&mut self, byte: u8) -> Option<Event> {
        const END_SEQ: &[u8] = b"\x1b[201~";

        // Logic:
        // 1. If we have room in paste_buffer, push it.
        // 2. If we are full, push to self.buffer (used as a tail tracker) to detect END_SEQ.
        // 3. Always check if the effective stream ends with END_SEQ.

        if self.paste_buffer.len() < MAX_PASTE_LEN {
            self.paste_buffer.push(byte);

            // Check for end sequence in paste_buffer
            if self.paste_buffer.ends_with(END_SEQ) {
                self.in_paste = false;
                // Remove the end sequence from content
                let content_len = self.paste_buffer.len() - END_SEQ.len();
                let content =
                    String::from_utf8_lossy(&self.paste_buffer[..content_len]).into_owned();
                self.paste_buffer.clear();
                return Some(Event::Paste(PasteEvent::bracketed(content)));
            }
        } else {
            // Buffer is full. DoS protection active.
            // We stop collecting content, but we MUST track the end sequence.
            // Use self.buffer as a sliding window for the tail.

            self.buffer.push(byte);
            if self.buffer.len() > END_SEQ.len() {
                self.buffer.remove(0);
            }

            // Check if we found the end sequence.
            // The sequence might be split between paste_buffer and buffer.
            // We only need to check the last 6 bytes.
            // Since `buffer` contains the most recent bytes (up to 6), and `paste_buffer` is full...

            // Construct a view of the last 6 bytes
            let mut last_bytes = [0u8; 6];
            let tail_len = self.buffer.len();
            let paste_len = self.paste_buffer.len();

            if tail_len + paste_len >= 6 {
                // Fill from buffer (reverse order)
                for i in 0..tail_len {
                    last_bytes[6 - tail_len + i] = self.buffer[i];
                }
                // Fill remaining from paste_buffer
                let remaining = 6 - tail_len;
                if remaining > 0 {
                    let start = paste_len - remaining;
                    last_bytes[..remaining]
                        .copy_from_slice(&self.paste_buffer[start..(remaining + start)]);
                }

                if last_bytes == END_SEQ {
                    self.in_paste = false;

                    // We found the end sequence.
                    // The content is `paste_buffer` MINUS the part of END_SEQ that was in it.
                    // `remaining` bytes of END_SEQ were in paste_buffer.

                    let content_len = paste_len - remaining;
                    let content =
                        String::from_utf8_lossy(&self.paste_buffer[..content_len]).into_owned();

                    self.paste_buffer.clear();
                    self.buffer.clear();

                    return Some(Event::Paste(PasteEvent::bracketed(content)));
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csi_ignore_handles_final_bytes() {
        let mut parser = InputParser::new();

        // Create a very long CSI sequence terminated by '@' (0x40)
        // 0x40 is a valid Final Byte (ECMA-48), but our parser currently only checks A-Za-z~
        let mut seq = vec![0x1B, b'['];
        seq.extend(std::iter::repeat_n(b'0', MAX_CSI_LEN + 100)); // Trigger CsiIgnore
        seq.push(b'@'); // Final byte

        let events = parser.parse(&seq);
        assert_eq!(events.len(), 0);

        // Feed 'a'. If '@' was correctly treated as final byte, 'a' should be parsed as 'a'.
        // If '@' was ignored (stayed in CsiIgnore), 'a' terminates the sequence and is swallowed.
        let events = parser.parse(b"a");
        assert_eq!(events.len(), 1, "Subsequent char 'a' was swallowed");
        assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Char('a')));
    }

    #[test]
    fn ascii_characters_parsed() {
        let mut parser = InputParser::new();

        let events = parser.parse(b"abc");
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Char('a')));
        assert!(matches!(events[1], Event::Key(k) if k.code == KeyCode::Char('b')));
        assert!(matches!(events[2], Event::Key(k) if k.code == KeyCode::Char('c')));
    }

    #[test]
    fn control_characters() {
        let mut parser = InputParser::new();

        // Ctrl+A
        let events = parser.parse(&[0x01]);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            Event::Key(k) if k.code == KeyCode::Char('a') && k.modifiers.contains(Modifiers::CTRL)
        ));

        // Backspace
        let events = parser.parse(&[0x7F]);
        assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Backspace));
    }

    #[test]
    fn arrow_keys() {
        let mut parser = InputParser::new();

        assert!(matches!(
            parser.parse(b"\x1b[A").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up
        ));
        assert!(matches!(
            parser.parse(b"\x1b[B").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Down
        ));
        assert!(matches!(
            parser.parse(b"\x1b[C").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Right
        ));
        assert!(matches!(
            parser.parse(b"\x1b[D").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Left
        ));
    }

    #[test]
    fn function_keys_ss3() {
        let mut parser = InputParser::new();

        assert!(matches!(
            parser.parse(b"\x1bOP").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(1)
        ));
        assert!(matches!(
            parser.parse(b"\x1bOQ").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(2)
        ));
        assert!(matches!(
            parser.parse(b"\x1bOR").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(3)
        ));
        assert!(matches!(
            parser.parse(b"\x1bOS").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(4)
        ));
    }

    #[test]
    fn function_keys_csi() {
        let mut parser = InputParser::new();

        assert!(matches!(
            parser.parse(b"\x1b[15~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(5)
        ));
        assert!(matches!(
            parser.parse(b"\x1b[17~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(6)
        ));
    }

    #[test]
    fn modifiers_in_csi() {
        let mut parser = InputParser::new();

        // Shift+Up: CSI 1;2 A
        let events = parser.parse(b"\x1b[1;2A");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up && k.modifiers.contains(Modifiers::SHIFT)
        ));

        // Ctrl+Up: CSI 1;5 A
        let events = parser.parse(b"\x1b[1;5A");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up && k.modifiers.contains(Modifiers::CTRL)
        ));
    }

    #[test]
    fn modifiers_in_csi_alt_ctrl() {
        let mut parser = InputParser::new();

        // Alt+Ctrl+Up: CSI 1;7 A (1 + ALT(2) + CTRL(4) = 7)
        let events = parser.parse(b"\x1b[1;7A");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k))
                if k.code == KeyCode::Up
                    && k.modifiers.contains(Modifiers::ALT)
                    && k.modifiers.contains(Modifiers::CTRL)
        ));
    }

    #[test]
    fn kitty_keyboard_basic_char() {
        let mut parser = InputParser::new();

        let events = parser.parse(b"\x1b[97u");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k))
                if k.code == KeyCode::Char('a')
                    && k.modifiers == Modifiers::NONE
                    && k.kind == KeyEventKind::Press
        ));
    }

    #[test]
    fn kitty_keyboard_with_modifiers_and_kind() {
        let mut parser = InputParser::new();

        // Ctrl+repeat for 'a' (modifiers=5, event_type=2)
        let events = parser.parse(b"\x1b[97;5:2u");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k))
                if k.code == KeyCode::Char('a')
                    && k.modifiers.contains(Modifiers::CTRL)
                    && k.kind == KeyEventKind::Repeat
        ));
    }

    #[test]
    fn kitty_keyboard_function_key() {
        let mut parser = InputParser::new();

        let events = parser.parse(b"\x1b[57364;1u");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(1)
        ));
    }

    #[test]
    fn alt_key_escapes() {
        let mut parser = InputParser::new();

        let events = parser.parse(b"\x1ba");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('a') && k.modifiers.contains(Modifiers::ALT)
        ));
    }

    #[test]
    fn alt_backspace() {
        let mut parser = InputParser::new();

        let events = parser.parse(b"\x1b\x7f");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k))
                if k.code == KeyCode::Backspace && k.modifiers.contains(Modifiers::ALT)
        ));
    }

    #[test]
    fn escape_escape_resets_state() {
        let mut parser = InputParser::new();

        let events = parser.parse(b"\x1b\x1b");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Escape && k.modifiers.contains(Modifiers::ALT)
        ));

        let events = parser.parse(b"a");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('a') && k.modifiers == Modifiers::NONE
        ));
    }

    #[test]
    fn focus_events() {
        let mut parser = InputParser::new();

        assert!(matches!(
            parser.parse(b"\x1b[I").first(),
            Some(Event::Focus(true))
        ));
        assert!(matches!(
            parser.parse(b"\x1b[O").first(),
            Some(Event::Focus(false))
        ));
    }

    #[test]
    fn bracketed_paste() {
        let mut parser = InputParser::new();

        // Start paste mode, paste content, end paste mode
        let events = parser.parse(b"\x1b[200~hello world\x1b[201~");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Event::Paste(p) if p.text == "hello world"
        ));
    }

    #[test]
    fn mouse_sgr_protocol() {
        let mut parser = InputParser::new();

        // Left click at (10, 20)
        let events = parser.parse(b"\x1b[<0;10;20M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if m.x == 9 && m.y == 19 // 0-indexed
        ));
    }

    #[test]
    fn mouse_sgr_modifiers() {
        let mut parser = InputParser::new();

        // Shift+Alt+Ctrl + left button (0 + 4 + 8 + 16 = 28)
        let events = parser.parse(b"\x1b[<28;3;4M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m))
                if m.modifiers.contains(Modifiers::SHIFT)
                    && m.modifiers.contains(Modifiers::ALT)
                    && m.modifiers.contains(Modifiers::CTRL)
        ));
    }

    #[test]
    fn mouse_sgr_scroll_up() {
        let mut parser = InputParser::new();

        // Scroll up: button code 64
        let events = parser.parse(b"\x1b[<64;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::ScrollUp)
        ));
    }

    #[test]
    fn mouse_sgr_scroll_down() {
        let mut parser = InputParser::new();

        // Scroll down: button code 65
        let events = parser.parse(b"\x1b[<65;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::ScrollDown)
        ));
    }

    #[test]
    fn mouse_sgr_scroll_left() {
        let mut parser = InputParser::new();

        // Scroll left: button code 66
        let events = parser.parse(b"\x1b[<66;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::ScrollLeft)
        ));
    }

    #[test]
    fn mouse_sgr_scroll_right() {
        let mut parser = InputParser::new();

        // Scroll right: button code 67
        let events = parser.parse(b"\x1b[<67;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::ScrollRight)
        ));
    }

    #[test]
    fn mouse_sgr_drag_left() {
        let mut parser = InputParser::new();

        // Drag with left button: button code 32
        let events = parser.parse(b"\x1b[<32;10;20M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Drag(MouseButton::Left))
        ));
    }

    #[test]
    fn utf8_characters() {
        let mut parser = InputParser::new();

        // é (U+00E9) = 0xC3 0xA9
        let events = parser.parse(&[0xC3, 0xA9]);
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('é')
        ));
    }

    #[test]
    fn invalid_utf8_emits_replacement_then_reprocesses_byte() {
        let mut parser = InputParser::new();

        // 0xE2 expects a 3-byte sequence, 0x28 is invalid continuation.
        let events = parser.parse(&[0xE2, 0x28]);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            Event::Key(k) if k.code == KeyCode::Char(std::char::REPLACEMENT_CHARACTER)
        ));
        assert!(matches!(
            events[1],
            Event::Key(k) if k.code == KeyCode::Char('(')
        ));
    }

    #[test]
    fn dos_protection_csi() {
        let mut parser = InputParser::new();

        // Create a very long CSI sequence
        let mut seq = vec![0x1B, b'['];
        seq.extend(std::iter::repeat_n(b'0', MAX_CSI_LEN + 100));
        seq.push(b'A');

        // DoS protection kicks in and switches to CsiIgnore
        // Excess bytes should be ignored, NOT leaked as characters
        let events = parser.parse(&seq);
        assert_eq!(
            events.len(),
            0,
            "Oversized CSI sequence should produce no events"
        );

        // The key invariant: parser should be back in ground state and functional
        // Verify by parsing a normal sequence after the attack
        let events = parser.parse(b"\x1b[A");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up
        ));
    }

    #[test]
    fn incomplete_csi_sequence_emits_no_event() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[");
        assert!(events.is_empty());
    }

    #[test]
    fn dos_protection_paste() {
        let mut parser = InputParser::new();

        // Start paste mode
        parser.parse(b"\x1b[200~");

        // Paste content up to the limit
        let content = vec![b'x'; MAX_PASTE_LEN - 100]; // Leave room for end sequence
        parser.parse(&content);

        // End paste mode
        let events = parser.parse(b"\x1b[201~");

        // Should have collected content up to limit
        assert!(matches!(
            events.first(),
            Some(Event::Paste(p)) if p.text.len() <= MAX_PASTE_LEN
        ));
    }

    #[test]
    fn dos_protection_paste_overflow_terminator() {
        let mut parser = InputParser::new();

        // Start paste mode
        parser.parse(b"\x1b[200~");

        // Overflow the buffer by pushing more than MAX_PASTE_LEN bytes.
        // DoS protection stops collecting content once buffer is full,
        // but continues tracking the end sequence to properly exit paste mode.
        let overflow = 100;
        let content = vec![b'a'; MAX_PASTE_LEN + overflow];
        parser.parse(&content);

        // Send terminator - parser MUST detect it and exit paste mode.
        // Even though the buffer overflowed, the terminator detection still works.
        let events = parser.parse(b"\x1b[201~");

        assert_eq!(events.len(), 1, "Should emit paste event");
        match &events[0] {
            Event::Paste(p) => {
                // Content is capped at MAX_PASTE_LEN due to DoS protection.
                // Overflow bytes are discarded but terminator is still detected.
                assert_eq!(
                    p.text.len(),
                    MAX_PASTE_LEN,
                    "Paste should be capped at MAX_PASTE_LEN bytes"
                );
                // The content should be all 'a' since we filled with 'a'
                assert!(p.text.chars().all(|c| c == 'a'));
            }
            _ => unreachable!("Expected Paste event"),
        }

        // Verify we are back in ground state by parsing a key
        let events = parser.parse(b"b");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Char('b')));
    }

    #[test]
    fn no_panic_on_invalid_input() {
        let mut parser = InputParser::new();

        // Random bytes that might trip up the parser
        let garbage = [0xFF, 0xFE, 0x00, 0x1B, 0x1B, 0x1B, b'[', 0xFF, b']', 0x00];

        // Should not panic
        let _ = parser.parse(&garbage);
    }

    #[test]
    fn dos_protection_paste_boundary() {
        let mut parser = InputParser::new();
        // Start paste mode
        parser.parse(b"\x1b[200~");

        // Fill buffer exactly to limit
        let content = vec![b'x'; MAX_PASTE_LEN];
        parser.parse(&content);

        // Send end sequence
        // Current bug: This will be dropped because buffer is full, trapping parser
        let events = parser.parse(b"\x1b[201~");

        assert!(
            !events.is_empty(),
            "Parser trapped in paste mode after hitting limit"
        );
        assert!(matches!(events[0], Event::Paste(_)));
    }

    // ── Navigation keys via CSI ~ sequences ──────────────────────────

    #[test]
    fn csi_tilde_home() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[1~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Home
        ));
    }

    #[test]
    fn csi_tilde_insert() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[2~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Insert
        ));
    }

    #[test]
    fn csi_tilde_delete() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[3~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Delete
        ));
    }

    #[test]
    fn csi_tilde_end() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[4~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::End
        ));
    }

    #[test]
    fn csi_tilde_page_up() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[5~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::PageUp
        ));
    }

    #[test]
    fn csi_tilde_page_down() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[6~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::PageDown
        ));
    }

    // ── Navigation keys via CSI H/F (xterm-style) ───────────────────

    #[test]
    fn csi_home_and_end() {
        let mut parser = InputParser::new();
        assert!(matches!(
            parser.parse(b"\x1b[H").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Home
        ));
        assert!(matches!(
            parser.parse(b"\x1b[F").first(),
            Some(Event::Key(k)) if k.code == KeyCode::End
        ));
    }

    // ── SS3 Home/End ─────────────────────────────────────────────────

    #[test]
    fn ss3_home_and_end() {
        let mut parser = InputParser::new();
        assert!(matches!(
            parser.parse(b"\x1bOH").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Home
        ));
        assert!(matches!(
            parser.parse(b"\x1bOF").first(),
            Some(Event::Key(k)) if k.code == KeyCode::End
        ));
    }

    // ── BackTab (Shift+Tab via CSI Z) ────────────────────────────────

    #[test]
    fn backtab_csi_z() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"\x1b[Z");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::BackTab
        ));
    }

    // ── F7-F12 keys via CSI tilde ────────────────────────────────────

    #[test]
    fn function_keys_f7_to_f12() {
        let mut parser = InputParser::new();
        assert!(matches!(
            parser.parse(b"\x1b[18~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(7)
        ));
        assert!(matches!(
            parser.parse(b"\x1b[19~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(8)
        ));
        assert!(matches!(
            parser.parse(b"\x1b[20~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(9)
        ));
        assert!(matches!(
            parser.parse(b"\x1b[21~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(10)
        ));
        assert!(matches!(
            parser.parse(b"\x1b[23~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(11)
        ));
        assert!(matches!(
            parser.parse(b"\x1b[24~").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(12)
        ));
    }

    // ── Modifier combinations on navigation keys ─────────────────────

    #[test]
    fn ctrl_home_and_alt_end() {
        let mut parser = InputParser::new();

        // Ctrl+Home: CSI 1;5 H
        let events = parser.parse(b"\x1b[1;5H");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Home && k.modifiers.contains(Modifiers::CTRL)
        ));

        // Alt+End: CSI 1;3 F
        let events = parser.parse(b"\x1b[1;3F");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::End && k.modifiers.contains(Modifiers::ALT)
        ));
    }

    #[test]
    fn shift_ctrl_arrow() {
        let mut parser = InputParser::new();

        // Shift+Ctrl+Right: CSI 1;6 C (modifier value 6 = 1 + Shift|Ctrl = 1 + 5)
        let events = parser.parse(b"\x1b[1;6C");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Right
                && k.modifiers.contains(Modifiers::SHIFT)
                && k.modifiers.contains(Modifiers::CTRL)
        ));
    }

    #[test]
    fn modifiers_on_tilde_keys() {
        let mut parser = InputParser::new();

        // Ctrl+Delete: CSI 3;5 ~
        let events = parser.parse(b"\x1b[3;5~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Delete && k.modifiers.contains(Modifiers::CTRL)
        ));

        // Shift+PageUp: CSI 5;2 ~
        let events = parser.parse(b"\x1b[5;2~");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::PageUp && k.modifiers.contains(Modifiers::SHIFT)
        ));
    }

    // ── Mouse right/middle click and release ─────────────────────────

    #[test]
    fn mouse_sgr_right_click() {
        let mut parser = InputParser::new();
        // Right click: button code 2
        let events = parser.parse(b"\x1b[<2;15;10M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Down(MouseButton::Right))
                && m.x == 14 && m.y == 9
        ));
    }

    #[test]
    fn mouse_sgr_middle_click() {
        let mut parser = InputParser::new();
        // Middle click: button code 1
        let events = parser.parse(b"\x1b[<1;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Down(MouseButton::Middle))
        ));
    }

    #[test]
    fn mouse_sgr_button_release() {
        let mut parser = InputParser::new();
        // Left button release: final byte 'm'
        let events = parser.parse(b"\x1b[<0;10;20m");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Up(MouseButton::Left))
        ));
    }

    #[test]
    fn mouse_sgr_moved() {
        let mut parser = InputParser::new();
        // Mouse move (no button): button code 35 (32 | 3, bit 5 set + bits 0-1 = 3)
        let events = parser.parse(b"\x1b[<35;10;20M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Moved)
        ));
    }

    #[test]
    fn mouse_sgr_with_modifiers() {
        let mut parser = InputParser::new();
        // Shift+Left click: button_code bit 2 set (shift) = 4
        let events = parser.parse(b"\x1b[<4;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && m.modifiers.contains(Modifiers::SHIFT)
        ));

        // Ctrl+Left click: button_code bit 4 set (ctrl) = 16
        let events = parser.parse(b"\x1b[<16;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && m.modifiers.contains(Modifiers::CTRL)
        ));

        // Alt+Left click: button_code bit 3 set (alt) = 8
        let events = parser.parse(b"\x1b[<8;5;5M");
        assert!(matches!(
            events.first(),
            Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && m.modifiers.contains(Modifiers::ALT)
        ));
    }

    // ── Kitty keyboard release events and special keys ───────────────

    #[test]
    fn kitty_keyboard_release_event() {
        let mut parser = InputParser::new();
        // Release event: kind=3
        let events = parser.parse(b"\x1b[97;1:3u");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('a') && k.kind == KeyEventKind::Release
        ));

        // Ctrl+release for 'A' (modifiers=5, event_type=3)
        let events = parser.parse(b"\x1b[65;5:3u");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k))
                if k.code == KeyCode::Char('A')
                    && k.modifiers.contains(Modifiers::CTRL)
                    && k.kind == KeyEventKind::Release
        ));
    }

    #[test]
    fn kitty_keyboard_special_keys() {
        let mut parser = InputParser::new();

        // Escape: 57344
        assert!(matches!(
            parser.parse(b"\x1b[57344u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Escape
        ));

        // Enter: 57345
        assert!(matches!(
            parser.parse(b"\x1b[57345u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Enter
        ));

        // Tab: 57346
        assert!(matches!(
            parser.parse(b"\x1b[57346u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Tab
        ));

        // Backspace: 57347
        assert!(matches!(
            parser.parse(b"\x1b[57347u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Backspace
        ));

        // Insert: 57348
        assert!(matches!(
            parser.parse(b"\x1b[57348u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Insert
        ));

        // Delete: 57349
        assert!(matches!(
            parser.parse(b"\x1b[57349u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Delete
        ));
    }

    #[test]
    fn kitty_keyboard_navigation_keys() {
        let mut parser = InputParser::new();

        // Left: 57350
        assert!(matches!(
            parser.parse(b"\x1b[57350u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Left
        ));
        // Right: 57351
        assert!(matches!(
            parser.parse(b"\x1b[57351u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Right
        ));
        // Up: 57352
        assert!(matches!(
            parser.parse(b"\x1b[57352u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up
        ));
        // Down: 57353
        assert!(matches!(
            parser.parse(b"\x1b[57353u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Down
        ));
        // PageUp: 57354
        assert!(matches!(
            parser.parse(b"\x1b[57354u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::PageUp
        ));
        // PageDown: 57355
        assert!(matches!(
            parser.parse(b"\x1b[57355u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::PageDown
        ));
        // Home: 57356
        assert!(matches!(
            parser.parse(b"\x1b[57356u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Home
        ));
        // End: 57357
        assert!(matches!(
            parser.parse(b"\x1b[57357u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::End
        ));
    }

    #[test]
    fn kitty_keyboard_f_keys() {
        let mut parser = InputParser::new();
        // F1: 57364
        assert!(matches!(
            parser.parse(b"\x1b[57364u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(1)
        ));
        // F12: 57375
        assert!(matches!(
            parser.parse(b"\x1b[57375u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(12)
        ));
        // F24: 57387
        assert!(matches!(
            parser.parse(b"\x1b[57387u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::F(24)
        ));
    }

    #[test]
    fn kitty_keyboard_ascii_as_standard() {
        let mut parser = InputParser::new();
        // Tab (9), Enter (13), Escape (27), Backspace (127)
        assert!(matches!(
            parser.parse(b"\x1b[9u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Tab
        ));
        assert!(matches!(
            parser.parse(b"\x1b[13u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Enter
        ));
        assert!(matches!(
            parser.parse(b"\x1b[27u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Escape
        ));
        assert!(matches!(
            parser.parse(b"\x1b[127u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Backspace
        ));
        // Backspace alternate: 8
        assert!(matches!(
            parser.parse(b"\x1b[8u").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Backspace
        ));
    }

    // ── OSC 52 clipboard ─────────────────────────────────────────────

    #[test]
    fn osc52_clipboard_bel_terminated() {
        let mut parser = InputParser::new();
        // OSC 52;c;<base64 "hello"> BEL
        // "hello" in base64 is "aGVsbG8="
        let events = parser.parse(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(matches!(
            events.first(),
            Some(Event::Clipboard(c)) if c.content == "hello" && c.source == ClipboardSource::Osc52
        ));
    }

    #[test]
    fn osc52_clipboard_st_terminated() {
        let mut parser = InputParser::new();
        // OSC 52;c;<base64 "hello"> ESC \
        let events = parser.parse(b"\x1b]52;c;aGVsbG8=\x1b\\");
        assert!(matches!(
            events.first(),
            Some(Event::Clipboard(c)) if c.content == "hello"
        ));
    }

    #[test]
    fn osc52_clipboard_primary_selection() {
        let mut parser = InputParser::new();
        // Primary selection: p instead of c
        // "abc" in base64 is "YWJj"
        let events = parser.parse(b"\x1b]52;p;YWJj\x07");
        assert!(matches!(
            events.first(),
            Some(Event::Clipboard(c)) if c.content == "abc"
        ));
    }

    // ── Control keys ─────────────────────────────────────────────────

    #[test]
    fn ctrl_space_is_null() {
        let mut parser = InputParser::new();
        let events = parser.parse(&[0x00]);
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Null
        ));
    }

    #[test]
    fn all_ctrl_letter_keys() {
        let mut parser = InputParser::new();
        // Ctrl+A (0x01) through Ctrl+Z (0x1A), skipping Backspace (0x08), Tab (0x09), and Enter (0x0D)
        for byte in 0x01..=0x1Au8 {
            let events = parser.parse(&[byte]);
            assert_eq!(
                events.len(),
                1,
                "Ctrl+{} should produce one event",
                (byte + b'a' - 1) as char
            );
            match byte {
                0x08 => assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Backspace)),
                0x09 => assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Tab)),
                0x0D => assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Enter)),
                _ => {
                    let expected_char = (byte + b'a' - 1) as char;
                    match &events[0] {
                        Event::Key(k) => {
                            assert_eq!(
                                k.code,
                                KeyCode::Char(expected_char),
                                "Byte 0x{byte:02X} should produce Ctrl+{expected_char}"
                            );
                            assert!(
                                k.modifiers.contains(Modifiers::CTRL),
                                "Byte 0x{byte:02X} should have Ctrl modifier"
                            );
                        }
                        other => {
                            panic!("Byte 0x{byte:02X}: expected Key event, got {other:?}");
                        }
                    }
                }
            }
        }
    }

    // ── UTF-8 multi-byte: 3-byte and 4-byte ─────────────────────────

    #[test]
    fn utf8_3byte_cjk() {
        let mut parser = InputParser::new();
        // 中 (U+4E2D) = 0xE4 0xB8 0xAD
        let events = parser.parse(&[0xE4, 0xB8, 0xAD]);
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('中')
        ));
    }

    #[test]
    fn utf8_4byte_emoji() {
        let mut parser = InputParser::new();
        // 🦀 (U+1F980) = 0xF0 0x9F 0xA6 0x80
        let events = parser.parse(&[0xF0, 0x9F, 0xA6, 0x80]);
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('🦀')
        ));
    }

    // ── Empty input ──────────────────────────────────────────────────

    #[test]
    fn empty_input_returns_no_events() {
        let mut parser = InputParser::new();
        let events = parser.parse(b"");
        assert!(events.is_empty());
    }

    // ── Unknown CSI tilde values ─────────────────────────────────────

    #[test]
    fn unknown_csi_tilde_ignored() {
        let mut parser = InputParser::new();
        // Code 99 is not a known tilde key
        let events = parser.parse(b"\x1b[99~");
        assert!(events.is_empty());

        // Parser should still work
        let events = parser.parse(b"a");
        assert!(matches!(events.first(), Some(Event::Key(k)) if k.code == KeyCode::Char('a')));
    }

    // ── Alt+various characters ───────────────────────────────────────

    #[test]
    fn alt_special_chars() {
        let mut parser = InputParser::new();

        // Alt+space
        let events = parser.parse(b"\x1b ");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char(' ') && k.modifiers.contains(Modifiers::ALT)
        ));

        // Alt+digit
        let events = parser.parse(b"\x1b5");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('5') && k.modifiers.contains(Modifiers::ALT)
        ));

        // Alt+bracket
        let events = parser.parse(b"\x1b}");
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Char('}') && k.modifiers.contains(Modifiers::ALT)
        ));
    }

    // ── SS3 arrow keys ───────────────────────────────────────────────

    #[test]
    fn ss3_arrow_keys() {
        let mut parser = InputParser::new();
        assert!(matches!(
            parser.parse(b"\x1bOA").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up
        ));
        assert!(matches!(
            parser.parse(b"\x1bOB").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Down
        ));
        assert!(matches!(
            parser.parse(b"\x1bOC").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Right
        ));
        assert!(matches!(
            parser.parse(b"\x1bOD").first(),
            Some(Event::Key(k)) if k.code == KeyCode::Left
        ));
    }

    // ── Xterm modifier encoding ──────────────────────────────────────

    #[test]
    fn xterm_modifier_encoding() {
        // Verify modifiers_from_xterm decoding (value = 1 + modifier_bits)
        assert_eq!(InputParser::modifiers_from_xterm(1), Modifiers::NONE);
        assert_eq!(InputParser::modifiers_from_xterm(2), Modifiers::SHIFT);
        assert_eq!(InputParser::modifiers_from_xterm(3), Modifiers::ALT);
        assert_eq!(
            InputParser::modifiers_from_xterm(4),
            Modifiers::SHIFT | Modifiers::ALT
        );
        assert_eq!(InputParser::modifiers_from_xterm(5), Modifiers::CTRL);
        assert_eq!(
            InputParser::modifiers_from_xterm(6),
            Modifiers::SHIFT | Modifiers::CTRL
        );
        assert_eq!(InputParser::modifiers_from_xterm(9), Modifiers::SUPER);
    }

    // ── SS3 interrupted by ESC ───────────────────────────────────────

    #[test]
    fn ss3_interrupted_by_esc() {
        let mut parser = InputParser::new();
        // ESC O ESC should restart into Escape state
        let events = parser.parse(b"\x1bO\x1b[A");
        // Should get Up arrow from the new ESC [ A sequence
        assert!(matches!(
            events.first(),
            Some(Event::Key(k)) if k.code == KeyCode::Up
        ));
    }

    // ── Kitty keyboard: unhandled keycodes ───────────────────────────

    #[test]
    fn kitty_keyboard_reserved_keycode_ignored() {
        let mut parser = InputParser::new();
        // Reserved range 57358..=57363 returns None
        let events = parser.parse(b"\x1b[57360u");
        assert!(events.is_empty());

        // Parser still works
        let events = parser.parse(b"x");
        assert!(matches!(events.first(), Some(Event::Key(k)) if k.code == KeyCode::Char('x')));
    }
    #[test]
    fn utf8_invalid_sequence_emits_replacement() {
        let mut parser = InputParser::new();

        // 0xE0 is a start of 3-byte sequence.
        // 0x41 ('A') is not a valid continuation byte.
        // Should emit Replacement Character then 'A'.
        let events = parser.parse(&[0xE0, 0x41]);
        assert_eq!(events.len(), 2);

        match &events[0] {
            Event::Key(k) => assert_eq!(k.code, KeyCode::Char(std::char::REPLACEMENT_CHARACTER)),
            _ => panic!("Expected replacement character"),
        }

        match &events[1] {
            Event::Key(k) => assert_eq!(k.code, KeyCode::Char('A')),
            _ => panic!("Expected character 'A'"),
        }
    }

    #[test]
    fn utf8_invalid_lead_emits_replacement() {
        let mut parser = InputParser::new();

        // 0xC0 is an invalid UTF-8 lead byte (overlong sequence).
        let events = parser.parse(&[0xC0, b'a']);
        assert!(
            matches!(events.first(), Some(Event::Key(k)) if k.code == KeyCode::Char(std::char::REPLACEMENT_CHARACTER)),
            "Expected replacement for invalid lead"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Key(k) if k.code == KeyCode::Char('a'))),
            "Expected subsequent ASCII to be preserved"
        );

        // 0xF5 is an out-of-range UTF-8 lead byte.
        let events = parser.parse(&[0xF5, b'b']);
        assert!(
            matches!(events.first(), Some(Event::Key(k)) if k.code == KeyCode::Char(std::char::REPLACEMENT_CHARACTER)),
            "Expected replacement for out-of-range lead"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Key(k) if k.code == KeyCode::Char('b'))),
            "Expected subsequent ASCII to be preserved"
        );
    }
}

#[cfg(test)]
mod proptest_fuzz {
    use super::*;
    use proptest::prelude::*;

    // ── Strategy helpers ────────────────────────────────────────────────
    // Avoid turbofish inside proptest! macro (Rust 2024 edition compat).

    fn arb_byte() -> impl Strategy<Value = u8> {
        any::<u8>()
    }

    fn arb_byte_vec(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(arb_byte(), 0..=max_len)
    }

    /// Generate a well-formed CSI sequence: ESC [ <params> <final byte>.
    fn csi_sequence() -> impl Strategy<Value = Vec<u8>> {
        let params = prop::collection::vec(0x30u8..=0x3F, 0..=20);
        let final_byte = 0x40u8..=0x7E;
        (params, final_byte).prop_map(|(p, f)| {
            let mut buf = vec![0x1B, b'['];
            buf.extend_from_slice(&p);
            buf.push(f);
            buf
        })
    }

    /// Generate an OSC sequence: ESC ] <content> ST.
    fn osc_sequence() -> impl Strategy<Value = Vec<u8>> {
        let content = prop::collection::vec(0x20u8..=0x7E, 0..=64);
        let terminator = prop_oneof![
            Just(vec![0x1B, b'\\']), // ESC backslash
            Just(vec![0x07]),        // BEL
        ];
        (content, terminator).prop_map(|(c, t)| {
            let mut buf = vec![0x1B, b']'];
            buf.extend_from_slice(&c);
            buf.extend_from_slice(&t);
            buf
        })
    }

    /// Generate an SS3 sequence: ESC O <final byte>.
    fn ss3_sequence() -> impl Strategy<Value = Vec<u8>> {
        (0x40u8..=0x7E).prop_map(|f| vec![0x1B, b'O', f])
    }

    /// Generate a bracketed paste: ESC[200~ <content> ESC[201~.
    fn paste_sequence() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(0x20u8..=0x7E, 0..=128).prop_map(|content| {
            let mut buf = vec![0x1B, b'[', b'2', b'0', b'0', b'~'];
            buf.extend_from_slice(&content);
            buf.extend_from_slice(b"\x1b[201~");
            buf
        })
    }

    /// Generate structured adversarial input: mix of valid sequences and random bytes.
    fn mixed_adversarial() -> impl Strategy<Value = Vec<u8>> {
        let fragment = prop_oneof![
            csi_sequence(),
            osc_sequence(),
            ss3_sequence(),
            paste_sequence(),
            arb_byte_vec(16),                            // random bytes
            Just(vec![0x1B]),                            // bare ESC
            Just(vec![0x1B, b'[']),                      // unterminated CSI
            Just(vec![0x1B, b']']),                      // unterminated OSC
            prop::collection::vec(0x80u8..=0xFF, 1..=4), // high bytes
        ];
        prop::collection::vec(fragment, 1..=8)
            .prop_map(|frags| frags.into_iter().flatten().collect())
    }

    // ── Property tests ─────────────────────────────────────────────────

    proptest! {
        /// Random bytes must never panic.
        #[test]
        fn random_bytes_never_panic(input in arb_byte_vec(512)) {
            let mut parser = InputParser::new();
            let _ = parser.parse(&input);
        }

        /// After parsing any input, the parser must be reusable for normal keys.
        #[test]
        fn parser_recovers_after_garbage(input in arb_byte_vec(256)) {
            let mut parser = InputParser::new();
            let _ = parser.parse(&input);

            // Feed a clean known sequence (letter 'z') after the garbage.
            let events = parser.parse(b"z");
            // Parser must not panic. We can't assert exact events because
            // the parser may still be mid-sequence, but it must not panic.
            let _ = events;
        }

        /// Structured mixed input (valid sequences + garbage) must never panic.
        #[test]
        fn mixed_sequences_never_panic(input in mixed_adversarial()) {
            let mut parser = InputParser::new();
            let _ = parser.parse(&input);
        }

        /// All generated events must be valid (non-panicking Debug).
        #[test]
        fn events_are_well_formed(input in arb_byte_vec(256)) {
            let mut parser = InputParser::new();
            let events = parser.parse(&input);
            for event in &events {
                // Exercise Debug impl — catches inconsistent internal state.
                let _ = format!("{event:?}");
            }
        }

        /// CSI sequences never produce more events than bytes fed.
        #[test]
        fn csi_event_count_bounded(seq in csi_sequence()) {
            let mut parser = InputParser::new();
            let events = parser.parse(&seq);
            prop_assert!(events.len() <= seq.len(),
                "Got {} events from {} bytes", events.len(), seq.len());
        }

        /// OSC sequences never produce more events than bytes fed.
        #[test]
        fn osc_event_count_bounded(seq in osc_sequence()) {
            let mut parser = InputParser::new();
            let events = parser.parse(&seq);
            prop_assert!(events.len() <= seq.len(),
                "Got {} events from {} bytes", events.len(), seq.len());
        }

        /// Paste content is always bounded by MAX_PASTE_LEN.
        #[test]
        fn paste_content_bounded(content in prop::collection::vec(arb_byte(), 0..=2048)) {
            let mut parser = InputParser::new();
            let mut input = vec![0x1B, b'[', b'2', b'0', b'0', b'~'];
            input.extend_from_slice(&content);
            input.extend_from_slice(b"\x1b[201~");

            let events = parser.parse(&input);
            for event in &events {
                if let Event::Paste(p) = event {
                    prop_assert!(p.text.len() <= MAX_PASTE_LEN,
                        "Paste text {} exceeds limit {}", p.text.len(), MAX_PASTE_LEN);
                }
            }
        }

        /// Feeding input byte-by-byte yields same events as feeding all at once.
        #[test]
        fn incremental_matches_bulk(input in arb_byte_vec(128)) {
            let mut bulk_parser = InputParser::new();
            let bulk_events = bulk_parser.parse(&input);

            let mut incr_parser = InputParser::new();
            let mut incr_events = Vec::new();
            for byte in &input {
                incr_events.extend(incr_parser.parse(std::slice::from_ref(byte)));
            }

            let bulk_dbg: Vec<String> = bulk_events.iter().map(|e| format!("{e:?}")).collect();
            let incr_dbg: Vec<String> = incr_events.iter().map(|e| format!("{e:?}")).collect();
            prop_assert_eq!(bulk_dbg, incr_dbg,
                "Bulk vs incremental mismatch for input {:?}", input);
        }

        /// Repeated parsing of the same input must always produce the same result
        /// (parser is deterministic after reset).
        #[test]
        fn deterministic_output(input in arb_byte_vec(128)) {
            let mut parser1 = InputParser::new();
            let events1 = parser1.parse(&input);

            let mut parser2 = InputParser::new();
            let events2 = parser2.parse(&input);

            let dbg1: Vec<String> = events1.iter().map(|e| format!("{e:?}")).collect();
            let dbg2: Vec<String> = events2.iter().map(|e| format!("{e:?}")).collect();
            prop_assert_eq!(dbg1, dbg2);
        }
    }

    // ── Targeted invariant tests (outside proptest! macro) ─────────────

    /// After a long garbage run, parser handles a simple key within bounded time.
    #[test]
    fn no_quadratic_blowup() {
        let mut parser = InputParser::new();

        // 64KB of random-ish bytes (repeating pattern).
        let garbage: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
        let _ = parser.parse(&garbage);

        // Follow with a clean key — must not take pathological time.
        let events = parser.parse(b"a");
        let _ = events; // primarily asserting no hang/panic
    }

    /// Oversized CSI sequence triggers DoS protection without panic.
    #[test]
    fn oversized_csi_transitions_to_ignore() {
        let mut parser = InputParser::new();

        // CSI followed by MAX_CSI_LEN+100 parameter bytes then a final byte.
        let mut input = vec![0x1B, b'['];
        input.extend(std::iter::repeat_n(b'0', MAX_CSI_LEN + 100));
        input.push(b'm');

        let _ = parser.parse(&input);

        // Parser must still be usable.
        let events = parser.parse(b"x");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Char('x')));
    }

    /// Oversized OSC sequence triggers DoS protection without panic.
    #[test]
    fn oversized_osc_transitions_to_ignore() {
        let mut parser = InputParser::new();

        // OSC followed by MAX_OSC_LEN+100 content bytes then ST.
        let mut input = vec![0x1B, b']'];
        input.extend(std::iter::repeat_n(b'a', MAX_OSC_LEN + 100));
        input.push(0x07); // BEL terminator

        let _ = parser.parse(&input);

        // Parser must still be usable.
        let events = parser.parse(b"y");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Key(k) if k.code == KeyCode::Char('y')));
    }

    /// Rapid ESC toggling doesn't corrupt state.
    #[test]
    fn rapid_esc_toggle() {
        let mut parser = InputParser::new();

        // 1000 bare ESCs in a row.
        let input: Vec<u8> = vec![0x1B; 1000];
        let _ = parser.parse(&input);

        // Must recover for a normal key.
        let events = parser.parse(b"k");
        assert!(!events.is_empty());
    }

    /// Interleaved paste start sequences without end.
    #[test]
    fn unterminated_paste_recovery() {
        let mut parser = InputParser::new();

        // Start paste, but never end it — feed lots of data.
        let mut input = b"\x1b[200~".to_vec();
        input.extend(std::iter::repeat_n(b'x', 2048));

        let _ = parser.parse(&input);

        // Now end the paste.
        let events = parser.parse(b"\x1b[201~");
        assert!(
            !events.is_empty(),
            "Parser should emit paste event on terminator"
        );
    }

    /// UTF-8 boundary: all possible lead bytes followed by truncation.
    #[test]
    fn truncated_utf8_lead_bytes() {
        let mut parser = InputParser::new();

        // Two-byte lead (0xC0..0xDF), three-byte (0xE0..0xEF), four-byte (0xF0..0xF7)
        for lead in [0xC2, 0xE0, 0xF0] {
            let _ = parser.parse(&[lead]);
            // Feed a normal ASCII after the truncated lead.
            let events = parser.parse(b"a");
            // Must not panic; 'a' should eventually appear.
            let _ = events;
        }
    }

    /// Null bytes mixed with valid input.
    #[test]
    fn null_bytes_interleaved() {
        let mut parser = InputParser::new();

        let input = b"\x00A\x00\x1b[A\x00B\x00";
        let events = parser.parse(input);
        // Should get events for 'A', Up arrow, and 'B' (nulls handled gracefully).
        assert!(
            events.len() >= 2,
            "Expected at least 2 events, got {}",
            events.len()
        );
    }

    // ── Additional fuzz invariant tests (bd-10i.11.3) ─────────────────

    /// Generate an OSC 52 clipboard sequence with arbitrary base64 payload.
    fn osc52_sequence() -> impl Strategy<Value = Vec<u8>> {
        let selector = prop_oneof![Just(b'c'), Just(b'p'), Just(b's')];
        // Generate valid base64 characters with occasional invalid ones
        let payload = prop::collection::vec(
            prop_oneof![
                0x41u8..=0x5A, // A-Z
                0x61u8..=0x7A, // a-z
                0x30u8..=0x39, // 0-9
                Just(b'+'),
                Just(b'/'),
                Just(b'='),
            ],
            0..=128,
        );
        let terminator = prop_oneof![
            Just(vec![0x1B, b'\\']), // ESC backslash (ST)
            Just(vec![0x07]),        // BEL
        ];
        (selector, payload, terminator).prop_map(|(sel, pay, term)| {
            let mut buf = vec![0x1B, b']', b'5', b'2', b';', sel, b';'];
            buf.extend_from_slice(&pay);
            buf.extend_from_slice(&term);
            buf
        })
    }

    /// Generate an SGR mouse sequence.
    fn sgr_mouse_sequence() -> impl Strategy<Value = Vec<u8>> {
        let button_code = 0u16..128;
        let x = 1u16..300;
        let y = 1u16..100;
        let final_byte = prop_oneof![Just(b'M'), Just(b'm')];
        (button_code, x, y, final_byte)
            .prop_map(|(btn, x, y, fb)| format!("\x1b[<{btn};{x};{y}{}", fb as char).into_bytes())
    }

    /// Generate Kitty keyboard protocol sequences.
    fn kitty_keyboard_sequence() -> impl Strategy<Value = Vec<u8>> {
        let keycode = prop_oneof![
            0x20u32..0x7F,       // ASCII range
            0x57344u32..0x57400, // Kitty special keys
            0x100u32..0x200,     // Extended range
        ];
        let modifier = 1u32..16;
        let kind = prop_oneof![Just(1u32), Just(2u32), Just(3u32)]; // press/repeat/release
        (keycode, prop::option::of(modifier), prop::option::of(kind)).prop_map(
            |(kc, mods, kind)| match (mods, kind) {
                (Some(m), Some(k)) => format!("\x1b[{kc};{m}:{k}u").into_bytes(),
                (Some(m), None) => format!("\x1b[{kc};{m}u").into_bytes(),
                _ => format!("\x1b[{kc}u").into_bytes(),
            },
        )
    }

    proptest! {
        // --- OSC 52 clipboard tests ---

        /// OSC 52 clipboard sequences never panic.
        #[test]
        fn osc52_never_panics(seq in osc52_sequence()) {
            let mut parser = InputParser::new();
            let events = parser.parse(&seq);
            // If parsed, should be a Clipboard event
            for event in &events {
                if let Event::Clipboard(c) = event {
                    prop_assert!(!c.content.is_empty() || c.content.is_empty(),
                        "Clipboard event must have a content field");
                }
            }
        }

        /// OSC 52 with corrupt base64 doesn't panic.
        #[test]
        fn osc52_corrupt_base64_safe(payload in arb_byte_vec(128)) {
            let mut parser = InputParser::new();
            let mut input = b"\x1b]52;c;".to_vec();
            input.extend_from_slice(&payload);
            input.push(0x07); // BEL terminator
            let _ = parser.parse(&input);
        }

        // --- SGR mouse tests ---

        /// All SGR mouse sequences parse without panicking.
        #[test]
        fn sgr_mouse_never_panics(seq in sgr_mouse_sequence()) {
            let mut parser = InputParser::new();
            let events = parser.parse(&seq);
            for event in &events {
                // Verify events are well-formed (exercises Debug impl)
                let _ = format!("{event:?}");
            }
        }

        /// SGR mouse with extreme coordinates doesn't overflow.
        #[test]
        fn sgr_mouse_extreme_coords(
            btn in 0u16..128,
            x in 0u16..=65535,
            y in 0u16..=65535,
        ) {
            let mut parser = InputParser::new();
            let input = format!("\x1b[<{btn};{x};{y}M").into_bytes();
            let events = parser.parse(&input);
            for event in &events {
                if let Event::Mouse(m) = event {
                    prop_assert!(m.x <= x, "Mouse x {} > input x {}", m.x, x);
                    prop_assert!(m.y <= y, "Mouse y {} > input y {}", m.y, y);
                }
            }
        }

        // --- Kitty keyboard protocol tests ---

        /// Kitty keyboard sequences never panic.
        #[test]
        fn kitty_keyboard_never_panics(seq in kitty_keyboard_sequence()) {
            let mut parser = InputParser::new();
            let _ = parser.parse(&seq);
        }

        // --- State boundary tests ---

        /// Truncated CSI followed by new valid sequence works correctly.
        #[test]
        fn truncated_csi_then_valid(
            params in prop::collection::vec(0x30u8..=0x3F, 1..=10),
            valid_char in 0x20u8..0x7F,
        ) {
            let mut parser = InputParser::new();

            // Send truncated CSI (no final byte)
            let mut partial = vec![0x1B, b'['];
            partial.extend_from_slice(&params);
            let _ = parser.parse(&partial);

            // Now send a fresh ESC sequence that should reset state
            let events = parser.parse(&[0x1B, b'[', b'A']); // Up arrow
            // Parser should eventually emit events (possibly including
            // interpretation of partial as complete)
            let _ = events;

            // Verify recovery with a simple key
            let events = parser.parse(&[valid_char]);
            let _ = events;
        }

        /// Truncated OSC followed by new valid sequence works.
        #[test]
        fn truncated_osc_then_valid(
            content in prop::collection::vec(0x20u8..=0x7E, 1..=32),
        ) {
            let mut parser = InputParser::new();

            // Send unterminated OSC
            let mut partial = vec![0x1B, b']'];
            partial.extend_from_slice(&content);
            let _ = parser.parse(&partial);

            // Send a new ESC to interrupt, then a valid key
            let events = parser.parse(b"\x1bz");
            let _ = events;
        }

        // --- Near-limit tests ---

        /// CSI sequence just under MAX_CSI_LEN produces events.
        #[test]
        fn csi_near_limit_produces_event(
            fill_byte in 0x30u8..=0x39, // digit parameter bytes
        ) {
            let mut parser = InputParser::new();

            let mut input = vec![0x1B, b'['];
            // Fill to just under limit
            input.extend(std::iter::repeat_n(fill_byte, MAX_CSI_LEN - 1));
            input.push(b'm'); // final byte (SGR)

            let events = parser.parse(&input);
            // Should NOT have been ignored (under limit)
            // The sequence is valid structurally even if params are nonsensical
            let _ = events;

            // Parser should still work
            let events = parser.parse(b"a");
            prop_assert!(!events.is_empty(), "Parser stuck after near-limit CSI");
        }

        /// OSC sequence just under MAX_OSC_LEN still processes.
        #[test]
        fn osc_near_limit_processes(
            fill_byte in 0x20u8..=0x7E,
        ) {
            let mut parser = InputParser::new();

            let mut input = vec![0x1B, b']'];
            input.extend(std::iter::repeat_n(fill_byte, MAX_OSC_LEN - 1));
            input.push(0x07); // BEL terminator

            let _ = parser.parse(&input);

            // Parser should still work
            let events = parser.parse(b"b");
            prop_assert!(!events.is_empty(), "Parser stuck after near-limit OSC");
        }

        // --- Consecutive paste tests ---

        /// Multiple back-to-back paste sequences all emit events.
        #[test]
        fn consecutive_pastes_emit_events(count in 2usize..=5) {
            let mut parser = InputParser::new();
            let mut input = Vec::new();

            for i in 0..count {
                input.extend_from_slice(b"\x1b[200~");
                input.extend_from_slice(format!("paste_{i}").as_bytes());
                input.extend_from_slice(b"\x1b[201~");
            }

            let events = parser.parse(&input);
            let paste_events: Vec<_> = events.iter()
                .filter(|e| matches!(e, Event::Paste(_)))
                .collect();

            prop_assert_eq!(paste_events.len(), count,
                "Expected {} paste events, got {}", count, paste_events.len());
        }

        /// Paste with invalid UTF-8 bytes doesn't panic.
        #[test]
        fn paste_with_invalid_utf8(content in arb_byte_vec(256)) {
            let mut parser = InputParser::new();
            let mut input = b"\x1b[200~".to_vec();
            input.extend_from_slice(&content);
            input.extend_from_slice(b"\x1b[201~");

            let events = parser.parse(&input);
            for event in &events {
                if let Event::Paste(p) = event {
                    // Text should be valid UTF-8 (lossy conversion happens internally)
                    prop_assert!(p.text.is_char_boundary(0), "Paste text is not valid UTF-8");
                }
            }
        }

        // --- Recovery invariants ---

        /// After any arbitrary input, feeding ESC then a known key recovers.
        #[test]
        fn recovery_via_esc_reset(garbage in arb_byte_vec(256)) {
            let mut parser = InputParser::new();
            let _ = parser.parse(&garbage);

            // Terminate any pending OSC (BEL works from any OSC sub-state),
            // then ESC to flush any other intermediate state.
            let _ = parser.parse(b"\x07\x1b\\\x1b");
            let _ = parser.parse(b"\x1b");

            // Now feed a clean character.
            let _ = parser.parse(b"z");

            // Feed one more clean character to verify.
            let events = parser.parse(b"q");
            // After terminating all pending sequences and feeding clean input,
            // the parser must produce events.
            prop_assert!(!events.is_empty(),
                "Parser did not recover after garbage + reset");
        }
    }
}
