#[cfg(test)]
mod tests {
    use ftui_core::event::{Event, KeyCode, KeyEvent, Modifiers};
    use ftui_core::input_parser::InputParser;

    #[test]
    fn test_alt_ctrl_key_parsing() {
        let mut parser = InputParser::new();
        // ESC (0x1B) + Ctrl+A (0x01) -> Should be Alt+Ctrl+A
        let events = parser.parse(&[0x1B, 0x01]);
        
        if events.is_empty() {
            panic!("BUG: Alt+Ctrl+A (0x1B 0x01) was swallowed!");
        }
        
        let event = &events[0];
        if let Event::Key(key) = event {
            // We expect Alt+Ctrl+A.
            // Ctrl+A is code 'a' with CTRL modifier.
            // ESC adds ALT modifier.
            // So code should be 'a', modifiers should be ALT | CTRL.
            assert_eq!(key.code, KeyCode::Char('a'), "Expected 'a' code");
            assert!(key.modifiers.contains(Modifiers::ALT), "Expected Alt modifier");
            assert!(key.modifiers.contains(Modifiers::CTRL), "Expected Ctrl modifier");
        } else {
            panic!("Expected Key event, got {:?}", event);
        }
    }
}
