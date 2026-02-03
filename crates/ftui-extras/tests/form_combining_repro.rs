#![cfg(feature = "forms")]

#[cfg(test)]
mod tests {
    use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_extras::forms::{Form, FormField, FormState};

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn test_form_combining_char_bug() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "e")]);
        let mut state = FormState::default();
        state.text_cursor = 1; // After 'e'

        // Insert combining acute accent (U+0301)
        state.handle_event(&mut form, &press(KeyCode::Char('\u{0301}')));

        if let Some(FormField::Text { value, .. }) = form.field(0) {
            // Value should be 'é' (1 grapheme)
            assert_eq!(value, "e\u{0301}");
            // BUG: cursor should be 1 (after 'é'), but logic likely made it 2
            assert_eq!(
                state.text_cursor, 1,
                "Cursor drifted after combining char insertion"
            );
        }
    }
}
