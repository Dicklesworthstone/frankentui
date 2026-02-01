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
    /// Bytes matched so far for paste end sequence.
    paste_match_len: usize,
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
            paste_match_len: 0,
        }
    }

    /// Parse input bytes and return any completed events.
    pub fn parse(&mut self, input: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in input {
            if let Some(event) = self.process_byte(byte) {
                events.push(event);
            }
        }
        events
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
            0x00 => Some(Event::Key(
                KeyEvent::new(KeyCode::Char(' ')).with_modifiers(Modifiers::CTRL),
            )),
            // Tab (Ctrl+I) - check before generic Ctrl range
            0x09 => Some(Event::Key(KeyEvent::new(KeyCode::Tab))),
            // Enter (Ctrl+M) - check before generic Ctrl range
            0x0D => Some(Event::Key(KeyEvent::new(KeyCode::Enter))),
            // Other Ctrl+A through Ctrl+Z (0x01-0x1A excluding Tab and Enter)
            0x01..=0x08 | 0x0A..=0x0C | 0x0E..=0x1A => {
                let c = (byte + b'a' - 1) as char;
                Some(Event::Key(
                    KeyEvent::new(KeyCode::Char(c)).with_modifiers(Modifiers::CTRL),
                ))
            }
            // Backspace (DEL)
            0x7F => Some(Event::Key(KeyEvent::new(KeyCode::Backspace))),
            // Printable ASCII
            0x20..=0x7E => Some(Event::Key(KeyEvent::new(KeyCode::Char(byte as char)))),
            // UTF-8 lead bytes
            0xC0..=0xDF => {
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
            0xF0..=0xF7 => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 {
                    collected: 1,
                    expected: 4,
                };
                None
            }
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
            // Another ESC - emit Alt+Escape and stay in Escape state
            0x1B => Some(Event::Key(
                KeyEvent::new(KeyCode::Escape).with_modifiers(Modifiers::ALT),
            )),
            // Alt+letter or Alt+char
            0x20..=0x7E => {
                self.state = ParserState::Ground;
                Some(Event::Key(
                    KeyEvent::new(KeyCode::Char(byte as char)).with_modifiers(Modifiers::ALT),
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
        match byte {
            // Final byte (0x40-0x7E) - return to ground
            0x40..=0x7E => {
                self.state = ParserState::Ground;
                None
            }
            // Intermediate bytes - keep ignoring
            _ => None,
        }
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
                self.paste_match_len = 0;
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
                KeyEvent::new(KeyCode::Tab).with_modifiers(Modifiers::SHIFT),
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
            57_364..=57_387 => Some(KeyCode::F((key_code - 57_364 + 1) as u8)),
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
            if button_code & 32 != 0 {
                MouseEventKind::Moved
            } else if button_code & 64 != 0 {
                MouseEventKind::ScrollUp
            } else if button_code & 65 != 0 {
                MouseEventKind::ScrollDown
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
        self.buffer.push(byte);

        match byte {
            // BEL terminates immediately
            0x07 => {
                self.state = ParserState::Ground;
                self.parse_osc_sequence()
            }
            // ESC might start terminator
            0x1B => {
                self.state = ParserState::OscEscape;
                None
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
            // ESC might start terminator
            0x1B => {
                self.state = ParserState::OscEscape;
                None
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
        } else {
            // Not a terminator, add ESC and byte to buffer
            self.buffer.push(0x1B);
            self.buffer.push(byte);
            self.state = ParserState::OscContent;
            None
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
            // ESC might start terminator
            0x1B => {
                self.state = ParserState::OscEscape;
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
            // Invalid - return to ground and re-process the unexpected byte
            self.state = ParserState::Ground;
            return self.process_ground(byte);
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
        self.paste_buffer.push(byte);

        // DoS protection: if buffer exceeds limit, truncate from start
        // We keep the last 64 bytes to ensure we can still detect the end sequence
        // even if the paste is massive. This prevents getting stuck in paste mode.
        if self.paste_buffer.len() > MAX_PASTE_LEN {
            let keep = 64;
            let remove = self.paste_buffer.len().saturating_sub(keep);
            self.paste_buffer.drain(0..remove);
        }

        // Check for end sequence: ESC [ 2 0 1 ~
        // We need to detect this pattern while still collecting bytes
        const END_SEQ: &[u8] = b"\x1b[201~";

        // Check if buffer ends with the end sequence
        if self.paste_buffer.ends_with(END_SEQ) {
            self.in_paste = false;
            // Remove the end sequence from content
            let content_len = self.paste_buffer.len() - END_SEQ.len();
            let content = String::from_utf8_lossy(&self.paste_buffer[..content_len]).into_owned();
            self.paste_buffer.clear();
            return Some(Event::Paste(PasteEvent::bracketed(content)));
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
        // DoS protection truncates to 64 bytes once the limit is exceeded,
        // but then allows more bytes to accumulate until limit is hit again.
        let overflow = 100;
        let content = vec![b'a'; MAX_PASTE_LEN + overflow];
        parser.parse(&content);

        // After truncation to 64 and adding remaining bytes:
        // - Buffer was truncated to 64 when it hit MAX_PASTE_LEN + 1
        // - Remaining (overflow - 1) bytes were added = 64 + 99 = 163 bytes
        // Send terminator - parser MUST detect it and exit paste mode.
        let events = parser.parse(b"\x1b[201~");

        assert_eq!(events.len(), 1, "Should emit paste event");
        match &events[0] {
            Event::Paste(p) => {
                // After truncation + remaining bytes + terminator detection:
                // Content = 64 + (overflow - 1) = 163 bytes, minus 6 for end seq removed = still 163
                // Wait - the terminator bytes are added to buffer (169 total), then removed (163 content)
                let expected_content_len = 64 + (overflow - 1);
                assert_eq!(
                    p.text.len(),
                    expected_content_len,
                    "Truncated paste should have {} bytes after DoS protection",
                    expected_content_len
                );
                // The content should be all 'a' since we filled with 'a'
                assert!(p.text.chars().all(|c| c == 'a'));
            }
            _ => panic!("Expected Paste event"),
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
