#![no_main]

use ftui_core::input_parser::InputParser;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|len: u16| {
    let count = (len as usize).min(16384);
    let mut data = Vec::with_capacity(count + 8);
    data.extend_from_slice(b"\x1b[");
    data.extend(std::iter::repeat(b'0').take(count));
    data.push(b'A');

    let mut parser = InputParser::new();
    let _ = parser.parse(&data);
    assert!(parser.dcs_buffer_len() <= ftui_core::input_parser::MAX_DCS_LEN);

    // Long DCS sequence with and without ST
    let mut dcs_data = Vec::with_capacity(count + 8);
    dcs_data.extend_from_slice(b"\x1bP");
    dcs_data.extend(std::iter::repeat(b'x').take(count));
    let _ = parser.parse(&dcs_data);
    assert!(parser.dcs_buffer_len() <= ftui_core::input_parser::MAX_DCS_LEN);
    let _ = parser.parse(b"\x1b\\");
    assert_eq!(parser.dcs_buffer_len(), 0);

    // Long APC sequence with capture enabled
    parser.set_capture_apc(true);
    let mut apc_data = Vec::with_capacity(count + 8);
    apc_data.extend_from_slice(b"\x1b_");
    apc_data.extend(std::iter::repeat(b'a').take(count));
    let _ = parser.parse(&apc_data);
    assert!(parser.dcs_buffer_len() <= ftui_core::input_parser::MAX_DCS_LEN);
    let _ = parser.parse(b"\x1b\\");
    assert_eq!(parser.dcs_buffer_len(), 0);
});
