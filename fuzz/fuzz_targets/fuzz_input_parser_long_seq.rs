#![no_main]

use ftui_core::input_parser::InputParser;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|len: u16| {
    let count = (len as usize).min(8192);
    let mut data = Vec::with_capacity(count + 3);
    data.extend_from_slice(b"\x1b[");
    data.extend(std::iter::repeat(b'0').take(count));
    data.push(b'A');

    let mut parser = InputParser::new();
    let _ = parser.parse(&data);
});
