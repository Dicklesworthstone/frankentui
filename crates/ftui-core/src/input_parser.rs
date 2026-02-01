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
    ClipboardEvent, ClipboardSource, Event, KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent,
    MouseEventKind, PasteEvent,
};

// Note: KeyEventKind import removed - we use the default Press kind

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
    /// After ESC O (SS3 introducer).
    Ss3,
    /// After ESC ] (OSC introducer).
    Osc,
    /// Collecting OSC content.
    OscContent,
    /// After ESC inside OSC (for ESC \ terminator).
    OscEscape,
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
            ParserState::Ss3 => self.process_ss3(byte),
            ParserState::Osc => self.process_osc(byte),
            ParserState::OscContent => self.process_osc_content(byte),
            ParserState::OscEscape => self.process_osc_escape(byte),
            ParserState::Utf8 { collected, expected } => {
                self.process_utf8(byte, collected, expected)
            }
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
            0x00 => Some(Event::Key(KeyEvent::new(KeyCode::Char(' ')).with_modifiers(Modifiers::CTRL))),
            // Tab (Ctrl+I) - check before generic Ctrl range
            0x09 => Some(Event::Key(KeyEvent::new(KeyCode::Tab))),
            // Enter (Ctrl+M) - check before generic Ctrl range
            0x0D => Some(Event::Key(KeyEvent::new(KeyCode::Enter))),
            // Other Ctrl+A through Ctrl+Z (0x01-0x1A excluding Tab and Enter)
            0x01..=0x08 | 0x0A..=0x0C | 0x0E..=0x1A => {
                let c = (byte + b'a' - 1) as char;
                Some(Event::Key(KeyEvent::new(KeyCode::Char(c)).with_modifiers(Modifiers::CTRL)))
            }
            // Backspace (DEL)
            0x7F => Some(Event::Key(KeyEvent::new(KeyCode::Backspace))),
            // Printable ASCII
            0x20..=0x7E => Some(Event::Key(KeyEvent::new(KeyCode::Char(byte as char)))),
            // UTF-8 lead bytes
            0xC0..=0xDF => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 { collected: 1, expected: 2 };
                None
            }
            0xE0..=0xEF => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 { collected: 1, expected: 3 };
                None
            }
            0xF0..=0xF7 => {
                self.utf8_buffer[0] = byte;
                self.state = ParserState::Utf8 { collected: 1, expected: 4 };
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
            0x1B => Some(Event::Key(KeyEvent::new(KeyCode::Escape).with_modifiers(Modifiers::ALT))),
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
            // Parameter bytes - continue collecting
            b'0'..=b'9' | b';' | b':' | b'<' | b'=' | b'>' | b'?' => {
                self.state = ParserState::CsiParam;
                None
            }
            // Final byte - parse and return
            b'A'..=b'Z' | b'a'..=b'z' | b'~' => {
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

    /// Process byte while collecting CSI parameters.
    fn process_csi_param(&mut self, byte: u8) -> Option<Event> {
        // DoS protection
        if self.buffer.len() >= MAX_CSI_LEN {
            self.state = ParserState::Ground;
            self.buffer.clear();
            return None;
        }

        self.buffer.push(byte);

        match byte {
            // Continue collecting parameters
            b'0'..=b'9' | b';' | b':' => None,
            // Final byte - parse and return (M and m are in A-Z and a-z ranges)
            b'A'..=b'Z' | b'a'..=b'z' | b'~' => {
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

        let modifier_value: u8 = s
            .split(';')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        // xterm modifier encoding: value = 1 + modifier_bits
        // Shift=1, Alt=2, Ctrl=4
        let bits = modifier_value.saturating_sub(1);
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
            self.state = ParserState::Ground;
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
            // Invalid - return to ground
            self.state = ParserState::Ground;
            return None;
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
        // DoS protection
        if self.paste_buffer.len() >= MAX_PASTE_LEN {
            // Silently drop excess content
            return None;
        }

        // Check for end sequence: ESC [ 2 0 1 ~
        // We need to detect this pattern while still collecting bytes
        const END_SEQ: &[u8] = b"\x1b[201~";

        self.paste_buffer.push(byte);

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

        assert!(matches!(parser.parse(b"\x1b[I").first(), Some(Event::Focus(true))));
        assert!(matches!(parser.parse(b"\x1b[O").first(), Some(Event::Focus(false))));
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

        // Should not panic - DoS protection kicks in and resets parser to ground
        // After reset, remaining bytes are parsed as ground state
        let _events = parser.parse(&seq);

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
    fn no_panic_on_invalid_input() {
        let mut parser = InputParser::new();

        // Random bytes that might trip up the parser
        let garbage = [
            0xFF, 0xFE, 0x00, 0x1B, 0x1B, 0x1B, b'[', 0xFF, b']', 0x00,
        ];

        // Should not panic
        let _ = parser.parse(&garbage);
    }
}
