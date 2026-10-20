#![no_main]

use arbitrary::Arbitrary;
use ftui_core::input_parser::InputParser;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
enum FuzzSequence {
    Char(u8),
    Escape,
    Csi(Vec<u8>),
    Osc(Vec<u8>),
    Dcs { payload: Vec<u8>, terminate_st: bool },
    Apc { payload: Vec<u8>, terminate_st: bool },
    Mouse { button: u8, x: u16, y: u16 },
    SgrPixelMouse {
        button: u8,
        px: u32,
        py: u32,
        released: bool,
    },
    XtwinopsCellSize { height: u16, width: u16 },
    Paste(Vec<u8>),
}

impl FuzzSequence {
    fn to_bytes(&self) -> Vec<u8> {
        match self {
            FuzzSequence::Char(b) => vec![*b],
            FuzzSequence::Escape => vec![0x1b],
            FuzzSequence::Csi(payload) => {
                let mut out = Vec::with_capacity(payload.len() + 3);
                out.extend_from_slice(b"\x1b[");
                out.extend(payload.iter().map(|b| b & 0x7f));
                out.push(b'm');
                out
            }
            FuzzSequence::Osc(payload) => {
                let mut out = Vec::with_capacity(payload.len() + 3);
                out.extend_from_slice(b"\x1b]");
                out.extend(payload.iter().map(|b| b & 0x7f));
                out.push(0x07);
                out
            }
            FuzzSequence::Dcs {
                payload,
                terminate_st,
            } => {
                let mut out = Vec::with_capacity(payload.len() + 4);
                out.extend_from_slice(b"\x1bP");
                out.extend_from_slice(payload);
                if *terminate_st {
                    out.extend_from_slice(b"\x1b\\");
                }
                out
            }
            FuzzSequence::Apc {
                payload,
                terminate_st,
            } => {
                let mut out = Vec::with_capacity(payload.len() + 4);
                out.extend_from_slice(b"\x1b_");
                out.extend_from_slice(payload);
                if *terminate_st {
                    out.extend_from_slice(b"\x1b\\");
                }
                out
            }
            FuzzSequence::Mouse { button, x, y } => {
                let mut out = Vec::new();
                let seq = format!("\x1b[<{};{};{}M", button, x, y);
                out.extend_from_slice(seq.as_bytes());
                out
            }
            FuzzSequence::SgrPixelMouse {
                button,
                px,
                py,
                released,
            } => {
                let final_char = if *released { 'm' } else { 'M' };
                format!("\x1b[<{};{};{}{}", button, px, py, final_char).into_bytes()
            }
            FuzzSequence::XtwinopsCellSize { height, width } => {
                format!("\x1b[6;{};{}t", height, width).into_bytes()
            }
            FuzzSequence::Paste(payload) => {
                let mut out = Vec::with_capacity(payload.len() + 10);
                out.extend_from_slice(b"\x1b[200~");
                out.extend(payload.iter().map(|b| b & 0x7f));
                out.extend_from_slice(b"\x1b[201~");
                out
            }
        }
    }
}

fuzz_target!(|input: (bool, bool, Option<(u16, u16)>, Vec<FuzzSequence>)| {
    let (pixel_mouse, capture_apc, initial_cell_size, sequences) = input;
    let mut buffer = Vec::new();
    for seq in sequences {
        if buffer.len() > 16384 {
            break;
        }
        buffer.extend_from_slice(&seq.to_bytes());
    }

    let mut parser = InputParser::new()
        .with_pixel_mouse(pixel_mouse)
        .with_capture_apc(capture_apc);
    if let Some((w, h)) = initial_cell_size {
        parser.set_cell_size_px(w, h);
    }
    let _ = parser.parse(&buffer);
    assert!(parser.dcs_buffer_len() <= ftui_core::input_parser::MAX_DCS_LEN);
});
