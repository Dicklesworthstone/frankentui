#![no_main]
#![forbid(unsafe_code)]

use ftui_render::sanitize::{SanitizeMode, sanitize_with};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let input = String::from_utf8_lossy(bytes);
    let plain = sanitize_with(&input, SanitizeMode::Strip);
    let styled = sanitize_with(&input, SanitizeMode::SgrOnly);
    assert_eq!(sanitize_with(&plain, SanitizeMode::Strip), plain);
    assert_eq!(sanitize_with(&styled, SanitizeMode::SgrOnly), styled);
    assert_eq!(sanitize_with(&styled, SanitizeMode::Strip), plain);
    assert_eq!(sanitize_with(&input, SanitizeMode::Raw), input);

    // Independent recognizer, not the production sequence scanner.
    let mut chars = styled.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            assert_eq!(chars.next(), Some('['));
            let mut length = 2;
            loop {
                let parameter = chars.next().expect("SGR must terminate");
                length += 1;
                assert!(length <= 64);
                if parameter == 'm' {
                    break;
                }
                assert!(parameter.is_ascii_digit() || matches!(parameter, ';' | ':'));
            }
        } else {
            assert!(!ch.is_control() || matches!(ch, '\t' | '\n' | '\r'));
        }
    }
    assert!(!plain.chars().any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n' | '\r')));
});
