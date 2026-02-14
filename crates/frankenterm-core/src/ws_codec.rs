#![allow(clippy::doc_markdown)] // protocol names like "HandshakeAck" in docs

//! WebSocket binary envelope codec for the FrankenTerm protocol.
//!
//! Implements encode/decode for the `frankenterm-ws-v1` binary frame format:
//!
//! ```text
//! +----------+----------+----------------------------+
//! | type (1) | len (3)  | payload (len bytes)        |
//! +----------+----------+----------------------------+
//! ```
//!
//! - **type**: 1-byte message type discriminator (0x01..=0x10).
//! - **len**: 3-byte big-endian unsigned payload length (max 16 MiB).
//! - **payload**: type-specific content (JSON or binary).
//!
//! # Feature gate
//!
//! This module requires the `ws-codec` feature (adds `serde` + `serde_json`).
//!
//! # Reference
//!
//! See `docs/spec/frankenterm-websocket-protocol.md` for the full specification.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Envelope header size: 1 byte type + 3 bytes length.
pub const HEADER_LEN: usize = 4;

/// Maximum payload length (3 bytes big-endian = 16 MiB - 1).
pub const MAX_PAYLOAD_LEN: usize = 0xFF_FF_FF;

/// Protocol version string used in handshake negotiation.
pub const PROTOCOL_VERSION: &str = "frankenterm-ws-v1";

// ---------------------------------------------------------------------------
// MessageType
// ---------------------------------------------------------------------------

/// Wire type discriminator for protocol messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageType {
    Handshake = 0x01,
    HandshakeAck = 0x02,
    Input = 0x03,
    Output = 0x04,
    Resize = 0x05,
    ResizeAck = 0x06,
    TerminalQuery = 0x07,
    TerminalReply = 0x08,
    FeatureToggle = 0x09,
    FeatureAck = 0x0A,
    Clipboard = 0x0B,
    Keepalive = 0x0C,
    KeepaliveAck = 0x0D,
    FlowControl = 0x0E,
    SessionEnd = 0x0F,
    Error = 0x10,
}

impl MessageType {
    /// Parse a raw byte into a known message type, or `None` for reserved codes.
    pub fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Handshake),
            0x02 => Some(Self::HandshakeAck),
            0x03 => Some(Self::Input),
            0x04 => Some(Self::Output),
            0x05 => Some(Self::Resize),
            0x06 => Some(Self::ResizeAck),
            0x07 => Some(Self::TerminalQuery),
            0x08 => Some(Self::TerminalReply),
            0x09 => Some(Self::FeatureToggle),
            0x0A => Some(Self::FeatureAck),
            0x0B => Some(Self::Clipboard),
            0x0C => Some(Self::Keepalive),
            0x0D => Some(Self::KeepaliveAck),
            0x0E => Some(Self::FlowControl),
            0x0F => Some(Self::SessionEnd),
            0x10 => Some(Self::Error),
            _ => None,
        }
    }

    /// Return the wire byte for this message type.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// ---------------------------------------------------------------------------
// Codec errors
// ---------------------------------------------------------------------------

/// Errors that can occur during frame encoding or decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Input buffer is shorter than the 4-byte header.
    BufferTooShort { available: usize, needed: usize },
    /// Payload length in header exceeds `MAX_PAYLOAD_LEN`.
    PayloadTooLarge { declared: usize },
    /// Input buffer does not contain the full frame (header + payload).
    Truncated { expected: usize, available: usize },
    /// Trailing bytes after the frame payload.
    TrailingBytes { frame_len: usize, buffer_len: usize },
    /// Unknown or reserved message type byte.
    UnknownMessageType { byte: u8 },
    /// JSON deserialization failed for a JSON-payload message.
    JsonError(String),
    /// Binary payload has wrong size for the message type.
    InvalidPayloadSize {
        msg_type: MessageType,
        expected: &'static str,
        actual: usize,
    },
    /// Invalid sub-type byte in an Input message.
    InvalidInputSubType { byte: u8 },
    /// Invalid semantic event kind byte.
    InvalidInputEventKind { byte: u8 },
}

impl core::fmt::Display for CodecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BufferTooShort { available, needed } => {
                write!(f, "buffer too short: {available} bytes, need {needed}")
            }
            Self::PayloadTooLarge { declared } => {
                write!(
                    f,
                    "payload too large: {declared} bytes (max {MAX_PAYLOAD_LEN})"
                )
            }
            Self::Truncated {
                expected,
                available,
            } => {
                write!(
                    f,
                    "truncated frame: expected {expected} bytes, got {available}"
                )
            }
            Self::TrailingBytes {
                frame_len,
                buffer_len,
            } => {
                write!(
                    f,
                    "trailing bytes: frame is {frame_len} bytes but buffer is {buffer_len}"
                )
            }
            Self::UnknownMessageType { byte } => {
                write!(f, "unknown message type: 0x{byte:02X}")
            }
            Self::JsonError(msg) => write!(f, "JSON error: {msg}"),
            Self::InvalidPayloadSize {
                msg_type,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "invalid payload size for {msg_type:?}: expected {expected}, got {actual}"
                )
            }
            Self::InvalidInputSubType { byte } => {
                write!(f, "invalid input sub-type: 0x{byte:02X}")
            }
            Self::InvalidInputEventKind { byte } => {
                write!(f, "invalid input event kind: 0x{byte:02X}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raw frame (envelope layer)
// ---------------------------------------------------------------------------

/// A decoded envelope: message type + raw payload bytes.
///
/// This is the low-level frame representation before typed payload parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
}

/// Encode a raw frame into the binary envelope format.
///
/// Returns `Err` if the payload exceeds `MAX_PAYLOAD_LEN`.
pub fn encode_raw(msg_type: MessageType, payload: &[u8]) -> Result<Vec<u8>, CodecError> {
    let len = payload.len();
    if len > MAX_PAYLOAD_LEN {
        return Err(CodecError::PayloadTooLarge { declared: len });
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + len);
    buf.push(msg_type.as_u8());
    // 3-byte big-endian length
    buf.push(((len >> 16) & 0xFF) as u8);
    buf.push(((len >> 8) & 0xFF) as u8);
    buf.push((len & 0xFF) as u8);
    buf.extend_from_slice(payload);
    Ok(buf)
}

/// Decode a single raw frame from a buffer that should contain exactly one frame.
///
/// Rejects truncated frames, unknown types, oversized payloads, and trailing bytes.
pub fn decode_raw(buf: &[u8]) -> Result<RawFrame, CodecError> {
    if buf.len() < HEADER_LEN {
        return Err(CodecError::BufferTooShort {
            available: buf.len(),
            needed: HEADER_LEN,
        });
    }

    let type_byte = buf[0];
    let msg_type = MessageType::from_u8(type_byte)
        .ok_or(CodecError::UnknownMessageType { byte: type_byte })?;

    let len = ((buf[1] as usize) << 16) | ((buf[2] as usize) << 8) | (buf[3] as usize);
    if len > MAX_PAYLOAD_LEN {
        return Err(CodecError::PayloadTooLarge { declared: len });
    }

    let total = HEADER_LEN + len;
    if buf.len() < total {
        return Err(CodecError::Truncated {
            expected: total,
            available: buf.len(),
        });
    }
    if buf.len() > total {
        return Err(CodecError::TrailingBytes {
            frame_len: total,
            buffer_len: buf.len(),
        });
    }

    Ok(RawFrame {
        msg_type,
        payload: buf[HEADER_LEN..total].to_vec(),
    })
}

/// Decode the next raw frame from the front of a buffer (streaming).
///
/// Returns `(frame, bytes_consumed)` on success. Unlike [`decode_raw`], this
/// does NOT reject trailing bytes — it's meant for stream parsing.
pub fn decode_raw_streaming(buf: &[u8]) -> Result<(RawFrame, usize), CodecError> {
    if buf.len() < HEADER_LEN {
        return Err(CodecError::BufferTooShort {
            available: buf.len(),
            needed: HEADER_LEN,
        });
    }

    let type_byte = buf[0];
    let msg_type = MessageType::from_u8(type_byte)
        .ok_or(CodecError::UnknownMessageType { byte: type_byte })?;

    let len = ((buf[1] as usize) << 16) | ((buf[2] as usize) << 8) | (buf[3] as usize);
    if len > MAX_PAYLOAD_LEN {
        return Err(CodecError::PayloadTooLarge { declared: len });
    }

    let total = HEADER_LEN + len;
    if buf.len() < total {
        return Err(CodecError::Truncated {
            expected: total,
            available: buf.len(),
        });
    }

    let frame = RawFrame {
        msg_type,
        payload: buf[HEADER_LEN..total].to_vec(),
    };
    Ok((frame, total))
}

// ---------------------------------------------------------------------------
// JSON-payload message types
// ---------------------------------------------------------------------------

/// Client capabilities declared in the handshake.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub clipboard: bool,
    #[serde(default)]
    pub osc_hyperlinks: bool,
    #[serde(default)]
    pub kitty_keyboard: bool,
    #[serde(default)]
    pub sixel: bool,
    #[serde(default)]
    pub truecolor: bool,
    #[serde(default)]
    pub bracketed_paste: bool,
    #[serde(default)]
    pub focus_events: bool,
    #[serde(default)]
    pub mouse_sgr: bool,
    #[serde(default)]
    pub unicode_version: Option<String>,
}

/// Terminal dimensions (cols x rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

/// Handshake message (0x01, Client -> Server, JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handshake {
    pub protocol_version: String,
    pub client_id: String,
    pub capabilities: Capabilities,
    pub initial_size: TerminalSize,
    #[serde(default)]
    pub dpr: Option<f64>,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub trace_mode: Option<bool>,
}

/// Flow control parameters from the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowControlParams {
    pub output_window: u32,
    pub input_window: u32,
    #[serde(default)]
    pub coalesce_resize_ms: Option<u32>,
    #[serde(default)]
    pub coalesce_mouse_move_ms: Option<u32>,
}

/// HandshakeAck message (0x02, Server -> Client, JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandshakeAck {
    pub protocol_version: String,
    pub session_id: String,
    pub server_id: String,
    pub effective_capabilities: Capabilities,
    pub term_profile: String,
    #[serde(default)]
    pub pty_pid: Option<u32>,
    pub flow_control: FlowControlParams,
}

/// Clipboard message (0x0B, bidirectional, JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipboardMsg {
    pub action: String,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub data_b64: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

/// SessionEnd message (0x0F, bidirectional, JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEndMsg {
    pub reason: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Error message (0x10, bidirectional, JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub code: String,
    pub message: String,
    pub fatal: bool,
}

// ---------------------------------------------------------------------------
// Binary-payload types
// ---------------------------------------------------------------------------

/// Input sub-type: raw bytes (0x00) or semantic event (0x01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPayload {
    /// Raw byte sequence forwarded to PTY stdin.
    Raw(Vec<u8>),
    /// Structured semantic input event.
    Semantic(SemanticInput),
}

/// Semantic input event (sub-type 0x01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticInput {
    pub kind: InputEventKind,
    pub modifiers: Modifiers,
    pub data: Vec<u8>,
}

/// Input event kind discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum InputEventKind {
    KeyDown = 0x01,
    KeyUp = 0x02,
    MouseDown = 0x03,
    MouseUp = 0x04,
    MouseMove = 0x05,
    MouseDrag = 0x06,
    Wheel = 0x07,
    Paste = 0x08,
    FocusIn = 0x09,
    FocusOut = 0x0A,
}

impl InputEventKind {
    pub fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::KeyDown),
            0x02 => Some(Self::KeyUp),
            0x03 => Some(Self::MouseDown),
            0x04 => Some(Self::MouseUp),
            0x05 => Some(Self::MouseMove),
            0x06 => Some(Self::MouseDrag),
            0x07 => Some(Self::Wheel),
            0x08 => Some(Self::Paste),
            0x09 => Some(Self::FocusIn),
            0x0A => Some(Self::FocusOut),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Modifier key bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const SHIFT: u8 = 1 << 0;
    pub const CTRL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const SUPER: u8 = 1 << 3;

    pub fn shift(self) -> bool {
        self.0 & Self::SHIFT != 0
    }
    pub fn ctrl(self) -> bool {
        self.0 & Self::CTRL != 0
    }
    pub fn alt(self) -> bool {
        self.0 & Self::ALT != 0
    }
    pub fn super_key(self) -> bool {
        self.0 & Self::SUPER != 0
    }
}

/// Output payload (0x04): PTY bytes with optional trace checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPayload {
    /// Raw PTY output bytes.
    pub pty_bytes: Vec<u8>,
    /// SHA-256 checksum of cumulative output (present when trace_mode is on).
    pub checksum: Option<[u8; 32]>,
}

// ---------------------------------------------------------------------------
// Typed message enum
// ---------------------------------------------------------------------------

/// A fully decoded protocol message with typed payload.
#[derive(Debug, Clone, PartialEq)]
pub enum WsMessage {
    Handshake(Handshake),
    HandshakeAck(HandshakeAck),
    Input(InputPayload),
    Output(OutputPayload),
    Resize(TerminalSize),
    ResizeAck(TerminalSize),
    TerminalQuery {
        seq_id: u16,
        query_bytes: Vec<u8>,
    },
    TerminalReply {
        seq_id: u16,
        reply_bytes: Vec<u8>,
    },
    FeatureToggle {
        features: u32,
    },
    FeatureAck {
        features: u32,
    },
    Clipboard(ClipboardMsg),
    Keepalive {
        timestamp_ns: u64,
    },
    KeepaliveAck {
        timestamp_ns: u64,
    },
    FlowControl {
        output_consumed: u32,
        input_consumed: u32,
    },
    SessionEnd(SessionEndMsg),
    Error(ErrorMsg),
}

// ---------------------------------------------------------------------------
// Encode typed messages
// ---------------------------------------------------------------------------

/// Encode a typed `WsMessage` into the binary envelope format.
pub fn encode(msg: &WsMessage) -> Result<Vec<u8>, CodecError> {
    match msg {
        WsMessage::Handshake(h) => {
            let json = serde_json::to_vec(h).map_err(|e| CodecError::JsonError(e.to_string()))?;
            encode_raw(MessageType::Handshake, &json)
        }
        WsMessage::HandshakeAck(h) => {
            let json = serde_json::to_vec(h).map_err(|e| CodecError::JsonError(e.to_string()))?;
            encode_raw(MessageType::HandshakeAck, &json)
        }
        WsMessage::Input(input) => {
            let payload = encode_input_payload(input);
            encode_raw(MessageType::Input, &payload)
        }
        WsMessage::Output(output) => {
            let payload = encode_output_payload(output);
            encode_raw(MessageType::Output, &payload)
        }
        WsMessage::Resize(size) => {
            let mut payload = [0u8; 4];
            payload[0..2].copy_from_slice(&size.cols.to_be_bytes());
            payload[2..4].copy_from_slice(&size.rows.to_be_bytes());
            encode_raw(MessageType::Resize, &payload)
        }
        WsMessage::ResizeAck(size) => {
            let mut payload = [0u8; 4];
            payload[0..2].copy_from_slice(&size.cols.to_be_bytes());
            payload[2..4].copy_from_slice(&size.rows.to_be_bytes());
            encode_raw(MessageType::ResizeAck, &payload)
        }
        WsMessage::TerminalQuery {
            seq_id,
            query_bytes,
        } => {
            let mut payload = Vec::with_capacity(2 + query_bytes.len());
            payload.extend_from_slice(&seq_id.to_be_bytes());
            payload.extend_from_slice(query_bytes);
            encode_raw(MessageType::TerminalQuery, &payload)
        }
        WsMessage::TerminalReply {
            seq_id,
            reply_bytes,
        } => {
            let mut payload = Vec::with_capacity(2 + reply_bytes.len());
            payload.extend_from_slice(&seq_id.to_be_bytes());
            payload.extend_from_slice(reply_bytes);
            encode_raw(MessageType::TerminalReply, &payload)
        }
        WsMessage::FeatureToggle { features } => {
            encode_raw(MessageType::FeatureToggle, &features.to_be_bytes())
        }
        WsMessage::FeatureAck { features } => {
            encode_raw(MessageType::FeatureAck, &features.to_be_bytes())
        }
        WsMessage::Clipboard(c) => {
            let json = serde_json::to_vec(c).map_err(|e| CodecError::JsonError(e.to_string()))?;
            encode_raw(MessageType::Clipboard, &json)
        }
        WsMessage::Keepalive { timestamp_ns } => {
            encode_raw(MessageType::Keepalive, &timestamp_ns.to_be_bytes())
        }
        WsMessage::KeepaliveAck { timestamp_ns } => {
            encode_raw(MessageType::KeepaliveAck, &timestamp_ns.to_be_bytes())
        }
        WsMessage::FlowControl {
            output_consumed,
            input_consumed,
        } => {
            let mut payload = [0u8; 8];
            payload[0..4].copy_from_slice(&output_consumed.to_be_bytes());
            payload[4..8].copy_from_slice(&input_consumed.to_be_bytes());
            encode_raw(MessageType::FlowControl, &payload)
        }
        WsMessage::SessionEnd(s) => {
            let json = serde_json::to_vec(s).map_err(|e| CodecError::JsonError(e.to_string()))?;
            encode_raw(MessageType::SessionEnd, &json)
        }
        WsMessage::Error(e) => {
            let json = serde_json::to_vec(e).map_err(|e| CodecError::JsonError(e.to_string()))?;
            encode_raw(MessageType::Error, &json)
        }
    }
}

fn encode_input_payload(input: &InputPayload) -> Vec<u8> {
    match input {
        InputPayload::Raw(bytes) => {
            let mut payload = Vec::with_capacity(1 + bytes.len());
            payload.push(0x00); // raw sub-type
            payload.extend_from_slice(bytes);
            payload
        }
        InputPayload::Semantic(sem) => {
            let data_len = sem.data.len() as u16;
            let mut payload = Vec::with_capacity(5 + sem.data.len());
            payload.push(0x01); // semantic sub-type
            payload.push(sem.kind.as_u8());
            payload.push(sem.modifiers.0);
            payload.extend_from_slice(&data_len.to_be_bytes());
            payload.extend_from_slice(&sem.data);
            payload
        }
    }
}

fn encode_output_payload(output: &OutputPayload) -> Vec<u8> {
    let has_checksum = output.checksum.is_some();
    let flags: u8 = if has_checksum { 0x01 } else { 0x00 };
    let checksum_len = if has_checksum { 32 } else { 0 };
    let mut payload = Vec::with_capacity(1 + output.pty_bytes.len() + checksum_len);
    payload.push(flags);
    payload.extend_from_slice(&output.pty_bytes);
    if let Some(ref checksum) = output.checksum {
        payload.extend_from_slice(checksum);
    }
    payload
}

// ---------------------------------------------------------------------------
// Decode typed messages
// ---------------------------------------------------------------------------

/// Decode a binary buffer (one complete WebSocket binary frame) into a typed
/// `WsMessage`.
///
/// The buffer must contain exactly one envelope frame (no trailing bytes).
pub fn decode(buf: &[u8]) -> Result<WsMessage, CodecError> {
    let raw = decode_raw(buf)?;
    decode_payload(raw)
}

/// Decode a `RawFrame` into a typed `WsMessage` by parsing its payload.
pub fn decode_payload(raw: RawFrame) -> Result<WsMessage, CodecError> {
    match raw.msg_type {
        MessageType::Handshake => {
            let h: Handshake = serde_json::from_slice(&raw.payload)
                .map_err(|e| CodecError::JsonError(e.to_string()))?;
            Ok(WsMessage::Handshake(h))
        }
        MessageType::HandshakeAck => {
            let h: HandshakeAck = serde_json::from_slice(&raw.payload)
                .map_err(|e| CodecError::JsonError(e.to_string()))?;
            Ok(WsMessage::HandshakeAck(h))
        }
        MessageType::Input => decode_input(&raw.payload),
        MessageType::Output => decode_output(&raw.payload),
        MessageType::Resize => {
            if raw.payload.len() != 4 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::Resize,
                    expected: "4 bytes",
                    actual: raw.payload.len(),
                });
            }
            let cols = u16::from_be_bytes([raw.payload[0], raw.payload[1]]);
            let rows = u16::from_be_bytes([raw.payload[2], raw.payload[3]]);
            Ok(WsMessage::Resize(TerminalSize { cols, rows }))
        }
        MessageType::ResizeAck => {
            if raw.payload.len() != 4 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::ResizeAck,
                    expected: "4 bytes",
                    actual: raw.payload.len(),
                });
            }
            let cols = u16::from_be_bytes([raw.payload[0], raw.payload[1]]);
            let rows = u16::from_be_bytes([raw.payload[2], raw.payload[3]]);
            Ok(WsMessage::ResizeAck(TerminalSize { cols, rows }))
        }
        MessageType::TerminalQuery => {
            if raw.payload.len() < 2 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::TerminalQuery,
                    expected: "at least 2 bytes",
                    actual: raw.payload.len(),
                });
            }
            let seq_id = u16::from_be_bytes([raw.payload[0], raw.payload[1]]);
            let query_bytes = raw.payload[2..].to_vec();
            Ok(WsMessage::TerminalQuery {
                seq_id,
                query_bytes,
            })
        }
        MessageType::TerminalReply => {
            if raw.payload.len() < 2 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::TerminalReply,
                    expected: "at least 2 bytes",
                    actual: raw.payload.len(),
                });
            }
            let seq_id = u16::from_be_bytes([raw.payload[0], raw.payload[1]]);
            let reply_bytes = raw.payload[2..].to_vec();
            Ok(WsMessage::TerminalReply {
                seq_id,
                reply_bytes,
            })
        }
        MessageType::FeatureToggle => {
            if raw.payload.len() != 4 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::FeatureToggle,
                    expected: "4 bytes",
                    actual: raw.payload.len(),
                });
            }
            let features = u32::from_be_bytes([
                raw.payload[0],
                raw.payload[1],
                raw.payload[2],
                raw.payload[3],
            ]);
            Ok(WsMessage::FeatureToggle { features })
        }
        MessageType::FeatureAck => {
            if raw.payload.len() != 4 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::FeatureAck,
                    expected: "4 bytes",
                    actual: raw.payload.len(),
                });
            }
            let features = u32::from_be_bytes([
                raw.payload[0],
                raw.payload[1],
                raw.payload[2],
                raw.payload[3],
            ]);
            Ok(WsMessage::FeatureAck { features })
        }
        MessageType::Clipboard => {
            let c: ClipboardMsg = serde_json::from_slice(&raw.payload)
                .map_err(|e| CodecError::JsonError(e.to_string()))?;
            Ok(WsMessage::Clipboard(c))
        }
        MessageType::Keepalive => {
            if raw.payload.len() != 8 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::Keepalive,
                    expected: "8 bytes",
                    actual: raw.payload.len(),
                });
            }
            let timestamp_ns = u64::from_be_bytes(raw.payload[0..8].try_into().unwrap());
            Ok(WsMessage::Keepalive { timestamp_ns })
        }
        MessageType::KeepaliveAck => {
            if raw.payload.len() != 8 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::KeepaliveAck,
                    expected: "8 bytes",
                    actual: raw.payload.len(),
                });
            }
            let timestamp_ns = u64::from_be_bytes(raw.payload[0..8].try_into().unwrap());
            Ok(WsMessage::KeepaliveAck { timestamp_ns })
        }
        MessageType::FlowControl => {
            if raw.payload.len() != 8 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::FlowControl,
                    expected: "8 bytes",
                    actual: raw.payload.len(),
                });
            }
            let output_consumed = u32::from_be_bytes(raw.payload[0..4].try_into().unwrap());
            let input_consumed = u32::from_be_bytes(raw.payload[4..8].try_into().unwrap());
            Ok(WsMessage::FlowControl {
                output_consumed,
                input_consumed,
            })
        }
        MessageType::SessionEnd => {
            let s: SessionEndMsg = serde_json::from_slice(&raw.payload)
                .map_err(|e| CodecError::JsonError(e.to_string()))?;
            Ok(WsMessage::SessionEnd(s))
        }
        MessageType::Error => {
            let e: ErrorMsg = serde_json::from_slice(&raw.payload)
                .map_err(|e| CodecError::JsonError(e.to_string()))?;
            Ok(WsMessage::Error(e))
        }
    }
}

fn decode_input(payload: &[u8]) -> Result<WsMessage, CodecError> {
    if payload.is_empty() {
        return Err(CodecError::InvalidPayloadSize {
            msg_type: MessageType::Input,
            expected: "at least 1 byte (sub-type)",
            actual: 0,
        });
    }
    match payload[0] {
        0x00 => {
            // Raw bytes
            Ok(WsMessage::Input(InputPayload::Raw(payload[1..].to_vec())))
        }
        0x01 => {
            // Semantic event: kind(1) + mods(1) + data_len(2) + data(variable)
            if payload.len() < 5 {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::Input,
                    expected: "at least 5 bytes for semantic input",
                    actual: payload.len(),
                });
            }
            let kind = InputEventKind::from_u8(payload[1])
                .ok_or(CodecError::InvalidInputEventKind { byte: payload[1] })?;
            let modifiers = Modifiers(payload[2]);
            let data_len = u16::from_be_bytes([payload[3], payload[4]]) as usize;
            let data_start = 5;
            let data_end = data_start + data_len;
            if payload.len() < data_end {
                return Err(CodecError::InvalidPayloadSize {
                    msg_type: MessageType::Input,
                    expected: "semantic data extends past payload",
                    actual: payload.len(),
                });
            }
            let data = payload[data_start..data_end].to_vec();
            Ok(WsMessage::Input(InputPayload::Semantic(SemanticInput {
                kind,
                modifiers,
                data,
            })))
        }
        other => Err(CodecError::InvalidInputSubType { byte: other }),
    }
}

fn decode_output(payload: &[u8]) -> Result<WsMessage, CodecError> {
    if payload.is_empty() {
        return Err(CodecError::InvalidPayloadSize {
            msg_type: MessageType::Output,
            expected: "at least 1 byte (flags)",
            actual: 0,
        });
    }
    let flags = payload[0];
    let has_checksum = flags & 0x01 != 0;
    if has_checksum {
        if payload.len() < 1 + 32 {
            return Err(CodecError::InvalidPayloadSize {
                msg_type: MessageType::Output,
                expected: "at least 33 bytes with checksum flag",
                actual: payload.len(),
            });
        }
        let checksum_start = payload.len() - 32;
        let pty_bytes = payload[1..checksum_start].to_vec();
        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(&payload[checksum_start..]);
        Ok(WsMessage::Output(OutputPayload {
            pty_bytes,
            checksum: Some(checksum),
        }))
    } else {
        let pty_bytes = payload[1..].to_vec();
        Ok(WsMessage::Output(OutputPayload {
            pty_bytes,
            checksum: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Envelope round-trip tests --

    #[test]
    fn raw_encode_decode_roundtrip() {
        let payload = b"hello world";
        let encoded = encode_raw(MessageType::Handshake, payload).unwrap();
        assert_eq!(encoded[0], 0x01); // type
        assert_eq!(encoded[1], 0x00); // len high
        assert_eq!(encoded[2], 0x00); // len mid
        assert_eq!(encoded[3], payload.len() as u8); // len low
        assert_eq!(&encoded[4..], payload);

        let decoded = decode_raw(&encoded).unwrap();
        assert_eq!(decoded.msg_type, MessageType::Handshake);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn raw_empty_payload() {
        let encoded = encode_raw(MessageType::Keepalive, &[]).unwrap();
        assert_eq!(encoded.len(), HEADER_LEN);
        let decoded = decode_raw(&encoded).unwrap();
        assert_eq!(decoded.msg_type, MessageType::Keepalive);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn raw_max_payload_length() {
        // Verify we can represent max length in header
        let payload = vec![0xAB; 256];
        let encoded = encode_raw(MessageType::Output, &payload).unwrap();
        let decoded = decode_raw(&encoded).unwrap();
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn raw_payload_too_large() {
        let payload = vec![0u8; MAX_PAYLOAD_LEN + 1];
        let result = encode_raw(MessageType::Output, &payload);
        assert!(matches!(result, Err(CodecError::PayloadTooLarge { .. })));
    }

    #[test]
    fn raw_buffer_too_short() {
        let result = decode_raw(&[0x01, 0x00]);
        assert!(matches!(result, Err(CodecError::BufferTooShort { .. })));
    }

    #[test]
    fn raw_truncated_payload() {
        // Header says 10 bytes but only 3 are present
        let buf = [0x01, 0x00, 0x00, 0x0A, 0xAA, 0xBB, 0xCC];
        let result = decode_raw(&buf);
        assert!(matches!(result, Err(CodecError::Truncated { .. })));
    }

    #[test]
    fn raw_trailing_bytes_rejected() {
        let mut encoded = encode_raw(MessageType::Keepalive, &[0x01; 8]).unwrap();
        encoded.push(0xFF); // trailing
        let result = decode_raw(&encoded);
        assert!(matches!(result, Err(CodecError::TrailingBytes { .. })));
    }

    #[test]
    fn raw_unknown_type_rejected() {
        let buf = [0x00, 0x00, 0x00, 0x00]; // type 0x00 is reserved
        let result = decode_raw(&buf);
        assert!(matches!(
            result,
            Err(CodecError::UnknownMessageType { byte: 0x00 })
        ));

        let buf2 = [0x11, 0x00, 0x00, 0x00]; // type 0x11 is reserved
        let result2 = decode_raw(&buf2);
        assert!(matches!(
            result2,
            Err(CodecError::UnknownMessageType { byte: 0x11 })
        ));
    }

    #[test]
    fn streaming_decode_multiple_frames() {
        let frame1 = encode_raw(MessageType::Resize, &[0x00, 0x78, 0x00, 0x28]).unwrap();
        let frame2 = encode_raw(MessageType::Keepalive, &[0u8; 8]).unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame1);
        buf.extend_from_slice(&frame2);

        let (f1, consumed1) = decode_raw_streaming(&buf).unwrap();
        assert_eq!(f1.msg_type, MessageType::Resize);
        assert_eq!(consumed1, frame1.len());

        let (f2, consumed2) = decode_raw_streaming(&buf[consumed1..]).unwrap();
        assert_eq!(f2.msg_type, MessageType::Keepalive);
        assert_eq!(consumed2, frame2.len());
    }

    // -- MessageType tests --

    #[test]
    fn message_type_roundtrip_all() {
        for byte in 0x01..=0x10u8 {
            let mt = MessageType::from_u8(byte).unwrap();
            assert_eq!(mt.as_u8(), byte);
        }
    }

    #[test]
    fn message_type_reserved_codes_are_none() {
        assert!(MessageType::from_u8(0x00).is_none());
        assert!(MessageType::from_u8(0x11).is_none());
        assert!(MessageType::from_u8(0xFF).is_none());
    }

    // -- Resize tests --

    #[test]
    fn resize_roundtrip() {
        let msg = WsMessage::Resize(TerminalSize {
            cols: 120,
            rows: 40,
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn resize_ack_roundtrip() {
        let msg = WsMessage::ResizeAck(TerminalSize { cols: 80, rows: 24 });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn resize_wrong_size_rejected() {
        let encoded = encode_raw(MessageType::Resize, &[0x00, 0x78, 0x00]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(
            result,
            Err(CodecError::InvalidPayloadSize {
                msg_type: MessageType::Resize,
                ..
            })
        ));
    }

    // -- Keepalive tests --

    #[test]
    fn keepalive_roundtrip() {
        let msg = WsMessage::Keepalive {
            timestamp_ns: 1_700_000_000_000_000_000,
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn keepalive_ack_roundtrip() {
        let msg = WsMessage::KeepaliveAck {
            timestamp_ns: 42_000_000,
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- FlowControl tests --

    #[test]
    fn flow_control_roundtrip() {
        let msg = WsMessage::FlowControl {
            output_consumed: 65536,
            input_consumed: 0,
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- Feature tests --

    #[test]
    fn feature_toggle_roundtrip() {
        let msg = WsMessage::FeatureToggle {
            features: 0b0000_1111,
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn feature_ack_roundtrip() {
        let msg = WsMessage::FeatureAck {
            features: 0b0000_0101,
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- TerminalQuery/Reply tests --

    #[test]
    fn terminal_query_roundtrip() {
        let msg = WsMessage::TerminalQuery {
            seq_id: 42,
            query_bytes: b"\x1b[6n".to_vec(), // cursor position query
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn terminal_reply_roundtrip() {
        let msg = WsMessage::TerminalReply {
            seq_id: 42,
            reply_bytes: b"\x1b[10;5R".to_vec(), // cursor at row 10, col 5
        };
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn terminal_query_too_short() {
        let encoded = encode_raw(MessageType::TerminalQuery, &[0x00]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(result, Err(CodecError::InvalidPayloadSize { .. })));
    }

    // -- Input tests --

    #[test]
    fn input_raw_roundtrip() {
        let msg = WsMessage::Input(InputPayload::Raw(b"hello\r\n".to_vec()));
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn input_semantic_keydown_roundtrip() {
        let msg = WsMessage::Input(InputPayload::Semantic(SemanticInput {
            kind: InputEventKind::KeyDown,
            modifiers: Modifiers(Modifiers::CTRL | Modifiers::SHIFT),
            data: b"KeyA".to_vec(),
        }));
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn input_semantic_mouse_down_roundtrip() {
        // button(1) + col(2) + row(2) = 5 bytes
        let mut data = Vec::new();
        data.push(0x00); // button 0 (left)
        data.extend_from_slice(&10u16.to_be_bytes()); // col 10
        data.extend_from_slice(&5u16.to_be_bytes()); // row 5
        let msg = WsMessage::Input(InputPayload::Semantic(SemanticInput {
            kind: InputEventKind::MouseDown,
            modifiers: Modifiers(0),
            data,
        }));
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn input_semantic_paste_roundtrip() {
        let msg = WsMessage::Input(InputPayload::Semantic(SemanticInput {
            kind: InputEventKind::Paste,
            modifiers: Modifiers(0),
            data: "pasted text content".as_bytes().to_vec(),
        }));
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn input_semantic_focus_in_roundtrip() {
        let msg = WsMessage::Input(InputPayload::Semantic(SemanticInput {
            kind: InputEventKind::FocusIn,
            modifiers: Modifiers(0),
            data: Vec::new(),
        }));
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn input_empty_payload_rejected() {
        let encoded = encode_raw(MessageType::Input, &[]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(result, Err(CodecError::InvalidPayloadSize { .. })));
    }

    #[test]
    fn input_invalid_subtype_rejected() {
        let encoded = encode_raw(MessageType::Input, &[0x02]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(
            result,
            Err(CodecError::InvalidInputSubType { byte: 0x02 })
        ));
    }

    #[test]
    fn input_semantic_truncated_rejected() {
        // sub-type 0x01 but only 3 bytes total (need at least 5)
        let encoded = encode_raw(MessageType::Input, &[0x01, 0x01, 0x00]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(result, Err(CodecError::InvalidPayloadSize { .. })));
    }

    #[test]
    fn input_semantic_invalid_kind_rejected() {
        let encoded = encode_raw(MessageType::Input, &[0x01, 0xFF, 0x00, 0x00, 0x00]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(
            result,
            Err(CodecError::InvalidInputEventKind { byte: 0xFF })
        ));
    }

    // -- Output tests --

    #[test]
    fn output_no_checksum_roundtrip() {
        let msg = WsMessage::Output(OutputPayload {
            pty_bytes: b"\x1b[31mhello\x1b[0m".to_vec(),
            checksum: None,
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn output_with_checksum_roundtrip() {
        let checksum = [0xABu8; 32];
        let msg = WsMessage::Output(OutputPayload {
            pty_bytes: b"data".to_vec(),
            checksum: Some(checksum),
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn output_empty_rejected() {
        let encoded = encode_raw(MessageType::Output, &[]).unwrap();
        let result = decode(&encoded);
        assert!(matches!(result, Err(CodecError::InvalidPayloadSize { .. })));
    }

    #[test]
    fn output_checksum_flag_but_too_short() {
        // flags=0x01 but only 10 bytes total (need at least 33)
        let encoded = encode_raw(
            MessageType::Output,
            &[
                0x01, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
            ],
        )
        .unwrap();
        let result = decode(&encoded);
        assert!(matches!(result, Err(CodecError::InvalidPayloadSize { .. })));
    }

    // -- JSON message tests --

    #[test]
    fn handshake_roundtrip() {
        let msg = WsMessage::Handshake(Handshake {
            protocol_version: PROTOCOL_VERSION.to_string(),
            client_id: "frankenterm-web/0.1.0".to_string(),
            capabilities: Capabilities {
                clipboard: true,
                osc_hyperlinks: true,
                kitty_keyboard: true,
                sixel: false,
                truecolor: true,
                bracketed_paste: true,
                focus_events: true,
                mouse_sgr: true,
                unicode_version: Some("15.1".to_string()),
            },
            initial_size: TerminalSize {
                cols: 120,
                rows: 40,
            },
            dpr: Some(2.0),
            auth_token: None,
            seed: Some(0),
            trace_mode: Some(false),
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn handshake_ack_roundtrip() {
        let msg = WsMessage::HandshakeAck(HandshakeAck {
            protocol_version: PROTOCOL_VERSION.to_string(),
            session_id: "01958c3a-test".to_string(),
            server_id: "ftui-remote/0.1.0".to_string(),
            effective_capabilities: Capabilities {
                clipboard: true,
                osc_hyperlinks: true,
                kitty_keyboard: false,
                sixel: false,
                truecolor: true,
                bracketed_paste: true,
                focus_events: true,
                mouse_sgr: true,
                unicode_version: None,
            },
            term_profile: "xterm-256color".to_string(),
            pty_pid: Some(12345),
            flow_control: FlowControlParams {
                output_window: 65536,
                input_window: 8192,
                coalesce_resize_ms: Some(50),
                coalesce_mouse_move_ms: Some(16),
            },
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn clipboard_roundtrip() {
        let msg = WsMessage::Clipboard(ClipboardMsg {
            action: "paste".to_string(),
            mime: Some("text/plain".to_string()),
            data_b64: Some("aGVsbG8gd29ybGQ=".to_string()),
            source: Some("clipboard".to_string()),
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn session_end_roundtrip() {
        let msg = WsMessage::SessionEnd(SessionEndMsg {
            reason: "pty_exit".to_string(),
            exit_code: Some(0),
            message: Some("shell exited normally".to_string()),
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn error_msg_roundtrip() {
        let msg = WsMessage::Error(ErrorMsg {
            code: "auth_failed".to_string(),
            message: "invalid bearer token".to_string(),
            fatal: true,
        });
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn invalid_json_rejected() {
        let encoded = encode_raw(MessageType::Handshake, b"not json").unwrap();
        let result = decode(&encoded);
        assert!(matches!(result, Err(CodecError::JsonError(_))));
    }

    // -- Modifier tests --

    #[test]
    fn modifier_bitfield() {
        let m = Modifiers(Modifiers::CTRL | Modifiers::ALT);
        assert!(!m.shift());
        assert!(m.ctrl());
        assert!(m.alt());
        assert!(!m.super_key());
    }

    // -- All-types encode/decode smoke test --

    #[test]
    fn all_message_types_roundtrip() {
        let messages = vec![
            WsMessage::Handshake(Handshake {
                protocol_version: PROTOCOL_VERSION.to_string(),
                client_id: "test".to_string(),
                capabilities: Capabilities {
                    clipboard: false,
                    osc_hyperlinks: false,
                    kitty_keyboard: false,
                    sixel: false,
                    truecolor: false,
                    bracketed_paste: false,
                    focus_events: false,
                    mouse_sgr: false,
                    unicode_version: None,
                },
                initial_size: TerminalSize { cols: 80, rows: 24 },
                dpr: None,
                auth_token: None,
                seed: None,
                trace_mode: None,
            }),
            WsMessage::HandshakeAck(HandshakeAck {
                protocol_version: PROTOCOL_VERSION.to_string(),
                session_id: "s1".to_string(),
                server_id: "srv".to_string(),
                effective_capabilities: Capabilities {
                    clipboard: false,
                    osc_hyperlinks: false,
                    kitty_keyboard: false,
                    sixel: false,
                    truecolor: false,
                    bracketed_paste: false,
                    focus_events: false,
                    mouse_sgr: false,
                    unicode_version: None,
                },
                term_profile: "xterm".to_string(),
                pty_pid: None,
                flow_control: FlowControlParams {
                    output_window: 65536,
                    input_window: 8192,
                    coalesce_resize_ms: None,
                    coalesce_mouse_move_ms: None,
                },
            }),
            WsMessage::Input(InputPayload::Raw(vec![0x61])),
            WsMessage::Output(OutputPayload {
                pty_bytes: vec![0x41],
                checksum: None,
            }),
            WsMessage::Resize(TerminalSize { cols: 80, rows: 24 }),
            WsMessage::ResizeAck(TerminalSize { cols: 80, rows: 24 }),
            WsMessage::TerminalQuery {
                seq_id: 1,
                query_bytes: vec![0x1b, 0x5b, 0x63],
            },
            WsMessage::TerminalReply {
                seq_id: 1,
                reply_bytes: vec![0x1b, 0x5b, 0x3f],
            },
            WsMessage::FeatureToggle { features: 0x0F },
            WsMessage::FeatureAck { features: 0x05 },
            WsMessage::Clipboard(ClipboardMsg {
                action: "copy".to_string(),
                mime: None,
                data_b64: None,
                source: None,
            }),
            WsMessage::Keepalive {
                timestamp_ns: 1_000_000,
            },
            WsMessage::KeepaliveAck {
                timestamp_ns: 1_000_000,
            },
            WsMessage::FlowControl {
                output_consumed: 4096,
                input_consumed: 0,
            },
            WsMessage::SessionEnd(SessionEndMsg {
                reason: "client_close".to_string(),
                exit_code: None,
                message: None,
            }),
            WsMessage::Error(ErrorMsg {
                code: "internal".to_string(),
                message: "test".to_string(),
                fatal: false,
            }),
        ];

        for (i, msg) in messages.iter().enumerate() {
            let encoded = encode(msg).unwrap_or_else(|e| panic!("encode #{i} failed: {e}"));
            let decoded = decode(&encoded).unwrap_or_else(|e| panic!("decode #{i} failed: {e}"));
            assert_eq!(&decoded, msg, "roundtrip failed for message #{i}");
        }
    }

    // -- Wire format verification --

    #[test]
    fn resize_wire_format_matches_spec() {
        // Spec: Resize (0x05), payload = cols(2, BE) + rows(2, BE) = 4 bytes
        let msg = WsMessage::Resize(TerminalSize {
            cols: 120,
            rows: 40,
        });
        let wire = encode(&msg).unwrap();
        // type=0x05, len=0x000004, cols=0x0078, rows=0x0028
        assert_eq!(wire, vec![0x05, 0x00, 0x00, 0x04, 0x00, 0x78, 0x00, 0x28]);
    }

    #[test]
    fn keepalive_wire_format_matches_spec() {
        let msg = WsMessage::Keepalive { timestamp_ns: 256 };
        let wire = encode(&msg).unwrap();
        // type=0x0C, len=0x000008, timestamp=0x0000000000000100
        assert_eq!(
            wire,
            vec![
                0x0C, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00
            ]
        );
    }

    #[test]
    fn flow_control_wire_format_matches_spec() {
        let msg = WsMessage::FlowControl {
            output_consumed: 0x0001_0000,
            input_consumed: 0,
        };
        let wire = encode(&msg).unwrap();
        assert_eq!(
            wire,
            vec![
                0x0E, 0x00, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn input_raw_wire_format() {
        let msg = WsMessage::Input(InputPayload::Raw(b"a".to_vec()));
        let wire = encode(&msg).unwrap();
        // type=0x03, len=0x000002, sub=0x00, data=0x61
        assert_eq!(wire, vec![0x03, 0x00, 0x00, 0x02, 0x00, 0x61]);
    }

    // -- Semantic input event kind tests --

    #[test]
    fn input_event_kind_roundtrip_all() {
        for byte in 0x01..=0x0Au8 {
            let kind = InputEventKind::from_u8(byte).unwrap();
            assert_eq!(kind.as_u8(), byte);
        }
    }

    #[test]
    fn input_event_kind_reserved_is_none() {
        assert!(InputEventKind::from_u8(0x00).is_none());
        assert!(InputEventKind::from_u8(0x0B).is_none());
        assert!(InputEventKind::from_u8(0xFF).is_none());
    }

    // -- Wheel event data format --

    #[test]
    fn input_semantic_wheel_roundtrip() {
        // dx(2,signed) + dy(2,signed) + col(2) + row(2) = 8 bytes
        let mut data = Vec::new();
        data.extend_from_slice(&(-1i16).to_be_bytes()); // dx
        data.extend_from_slice(&3i16.to_be_bytes()); // dy
        data.extend_from_slice(&50u16.to_be_bytes()); // col
        data.extend_from_slice(&10u16.to_be_bytes()); // row
        let msg = WsMessage::Input(InputPayload::Semantic(SemanticInput {
            kind: InputEventKind::Wheel,
            modifiers: Modifiers(0),
            data,
        }));
        let encoded = encode(&msg).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }
}
