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
    Mouse { button: u8, x: u16, y: u16 },
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
            FuzzSequence::Mouse { button, x, y } => {
                let mut out = Vec::new();
                let seq = format!("\x1b[<{};{};{}M", button, x, y);
                out.extend_from_slice(seq.as_bytes());
                out
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

fuzz_target!(|input: Vec<FuzzSequence>| {
    let mut buffer = Vec::new();
    for seq in input {
        if buffer.len() > 4096 {
            break;
        }
        buffer.extend_from_slice(&seq.to_bytes());
    }

    let mut parser = InputParser::new();
    let _ = parser.parse(&buffer);
});
