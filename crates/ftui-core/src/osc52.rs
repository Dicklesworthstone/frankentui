//! OSC 52 clipboard encoding shared by terminal output and clipboard backends.

use base64::{Engine as _, engine::general_purpose::STANDARD};

/// Common maximum base64 payload length (excluding the OSC envelope).
pub const MAX_OSC52_PAYLOAD: usize = 74_994;

/// Destination selection for an OSC 52 request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    /// System clipboard.
    Clipboard,
    /// X11 primary selection.
    Primary,
    /// X11 secondary selection.
    Secondary,
    /// Cut buffer 0 through 7.
    CutBuffer(u8),
}

impl ClipboardSelection {
    /// Validate the selection and return its wire selector.
    pub fn osc52_code(self) -> Result<char, Osc52Error> {
        match self {
            Self::Clipboard => Ok('c'),
            Self::Primary => Ok('p'),
            Self::Secondary => Ok('s'),
            Self::CutBuffer(index @ 0..=7) => Ok(char::from(b'0' + index)),
            Self::CutBuffer(index) => Err(Osc52Error::InvalidSelection(index)),
        }
    }
}

/// Invalid OSC 52 request; no bytes have been written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osc52Error {
    /// The encoded payload would exceed the configured limit.
    PayloadTooLarge {
        /// Encoded size, saturated if the calculation overflows.
        len: usize,
        /// Configured encoded size limit.
        max: usize,
    },
    /// Cut buffer index is outside 0 through 7.
    InvalidSelection(u8),
}

impl std::fmt::Display for Osc52Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadTooLarge { len, max } => {
                write!(f, "OSC 52 payload too large ({len} > {max})")
            }
            Self::InvalidSelection(index) => write!(f, "cut buffer index must be 0..=7 (got {index})"),
        }
    }
}

impl std::error::Error for Osc52Error {}

/// Encode a clipboard write, rejecting oversized payloads before allocating.
pub fn encode_set(selection: ClipboardSelection, bytes: &[u8]) -> Result<Vec<u8>, Osc52Error> {
    encode_set_with_limit(selection, bytes, MAX_OSC52_PAYLOAD)
}

/// Encode a write with an explicit maximum base64 length.
///
/// Used by clipboard backends that allow a terminal-specific payload limit.
pub fn encode_set_with_limit(
    selection: ClipboardSelection,
    bytes: &[u8],
    max: usize,
) -> Result<Vec<u8>, Osc52Error> {
    let code = selection.osc52_code()?;
    let len = bytes.len().div_ceil(3).saturating_mul(4);
    if len > max {
        return Err(Osc52Error::PayloadTooLarge { len, max });
    }
    let encoded = STANDARD.encode(bytes);
    Ok(format!("\x1b]52;{code};{encoded}\x07").into_bytes())
}

/// Encode a read request. Replies arrive asynchronously as clipboard events.
pub fn encode_query(selection: ClipboardSelection) -> Result<Vec<u8>, Osc52Error> {
    let code = selection.osc52_code()?;
    Ok(format!("\x1b]52;{code};?\x07").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_wire_format_and_selections() {
        assert_eq!(encode_set(ClipboardSelection::Clipboard, b"hello").unwrap(), b"\x1b]52;c;aGVsbG8=\x07");
        for (selection, code) in [
            (ClipboardSelection::Clipboard, 'c'),
            (ClipboardSelection::Primary, 'p'),
            (ClipboardSelection::Secondary, 's'),
            (ClipboardSelection::CutBuffer(0), '0'),
            (ClipboardSelection::CutBuffer(7), '7'),
        ] {
            assert_eq!(encode_query(selection).unwrap(), format!("\x1b]52;{code};?\x07").as_bytes());
            assert_eq!(encode_set(selection, b"").unwrap(), format!("\x1b]52;{code};\x07").as_bytes());
        }
        for index in [8, 255] {
            assert_eq!(encode_query(ClipboardSelection::CutBuffer(index)), Err(Osc52Error::InvalidSelection(index)));
            assert_eq!(encode_set(ClipboardSelection::CutBuffer(index), b"x"), Err(Osc52Error::InvalidSelection(index)));
        }
    }

    #[test]
    fn osc52_payload_cap_and_mux_wire_format() {
        let selection = ClipboardSelection::Clipboard;
        assert!(encode_set_with_limit(selection, b"abc", 4).is_ok());
        assert_eq!(encode_set_with_limit(selection, b"abcd", 4), Err(Osc52Error::PayloadTooLarge { len: 8, max: 4 }));
        assert!(encode_set(selection, &vec![0; 56_244]).is_ok());
        assert!(matches!(encode_set(selection, &vec![0; 56_245]), Err(Osc52Error::PayloadTooLarge { .. })));
        let seq = encode_set(selection, b"hello").unwrap();
        let mut tmux = Vec::new();
        crate::mux_passthrough::tmux_wrap(&mut tmux, &seq).unwrap();
        assert_eq!(tmux, b"\x1bPtmux;\x1b\x1b]52;c;aGVsbG8=\x07\x1b\\");
        let mut screen = Vec::new();
        crate::mux_passthrough::screen_wrap(&mut screen, &seq).unwrap();
        assert_eq!(screen, b"\x1bP\x1b]52;c;aGVsbG8=\x07\x1b\\");
    }
}
