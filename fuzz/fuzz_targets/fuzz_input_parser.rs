#![no_main]

use ftui_core::input_parser::InputParser;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut parser = InputParser::new();
    let _ = parser.parse(data);
});
