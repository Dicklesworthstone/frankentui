use ftui_text::wrap::{WrapMode, wrap_text};

#[test]
fn wrap_double_space_behavior() {
    // Width 5. "hello" fits. "  " (2 chars) makes it 7 > 5.
    let text = "hello  world";
    let lines = wrap_text(text, 5, WrapMode::Word);

    // Inter-word whitespace that would overflow is discarded at wrap.
    assert_eq!(lines, vec!["hello", "world"]);
}

#[test]
fn wrap_double_space_preserve_indent() {
    let text = "hello  world";
    let options = ftui_text::wrap::WrapOptions::new(5)
        .preserve_indent(true)
        .trim_trailing(true)
        .mode(WrapMode::Word);

    // preserve_indent applies to explicit paragraph-leading indent, not wrapped
    // inter-word spacing.
    let result = ftui_text::wrap::wrap_with_options(text, &options);
    assert_eq!(result, vec!["hello", "world"]);
}
