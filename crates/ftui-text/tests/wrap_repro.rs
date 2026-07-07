use ftui_text::wrap::{WrapMode, WrapOptions, wrap_text, wrap_text_optimal};

#[test]
fn test_wrap_optimal_indentation() {
    // "   foo" (3 spaces + 3 chars). Optimal mode treats whitespace as
    // paragraph glue: leading indentation is dropped, matching greedy
    // Word-mode defaults (see WrapMode::Optimal docs).
    let text = "   foo";

    // Width 10: single line, indent dropped (greedy Word agrees).
    let lines = wrap_text_optimal(text, 10);
    assert_eq!(lines, vec!["foo"]);
    assert_eq!(lines, wrap_text(text, 10, WrapMode::Word));

    // Width 4: historically the indent became an empty-content token and
    // the result was ["", "foo"] with maximal first-line badness — a
    // spurious blank line strictly worse than greedy. Now: ["foo"].
    let lines_narrow = wrap_text_optimal(text, 4);
    assert_eq!(lines_narrow, vec!["foo"]);
    assert_eq!(lines_narrow, wrap_text(text, 4, WrapMode::Word));
}

#[test]
fn test_wrap_word_indentation() {
    let text = "   foo";
    // Standard wrap: default preserve_indent is false, indent dropped.
    let lines = wrap_text(text, 10, WrapMode::Word);
    assert_eq!(lines, vec!["foo"]);

    // preserve_indent keeps the leading whitespace when it fits with the word.
    let opts = WrapOptions::new(10).preserve_indent(true);
    let lines_preserve = ftui_text::wrap::wrap_with_options(text, &opts);
    assert_eq!(lines_preserve, vec!["   foo"]);
}
