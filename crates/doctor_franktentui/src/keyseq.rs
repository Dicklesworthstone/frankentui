use crate::util::{duration_literal, tape_escape};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedToken {
    pub line: String,
    pub is_sleep: bool,
}

#[must_use]
pub fn emit_token(raw: &str) -> EmittedToken {
    let token = raw.trim();
    let lower = token.to_ascii_lowercase();

    if lower.starts_with("sleep:") || lower.starts_with("wait:") {
        let duration = token.split_once(':').map_or("0", |(_, value)| value).trim();
        return EmittedToken {
            line: format!("Sleep {}", duration_literal(duration)),
            is_sleep: true,
        };
    }

    let mapped = match lower.as_str() {
        "tab" => Some("Tab".to_string()),
        "enter" => Some("Enter".to_string()),
        "esc" | "escape" => Some("Escape".to_string()),
        "up" => Some("Up".to_string()),
        "down" => Some("Down".to_string()),
        "left" => Some("Left".to_string()),
        "right" => Some("Right".to_string()),
        "pageup" => Some("PageUp".to_string()),
        "pagedown" => Some("PageDown".to_string()),
        "ctrl+c" | "ctrl-c" => Some("Ctrl+C".to_string()),
        _ => None,
    };

    if let Some(line) = mapped {
        return EmittedToken {
            line,
            is_sleep: false,
        };
    }

    if lower.starts_with("text:") {
        let text = token
            .split_once(':')
            .map_or("", |(_, value)| value)
            .to_string();
        return EmittedToken {
            line: format!("Type \"{}\"", tape_escape(&text)),
            is_sleep: false,
        };
    }

    if token.chars().count() == 1 {
        return EmittedToken {
            line: format!("Type \"{}\"", tape_escape(token)),
            is_sleep: false,
        };
    }

    EmittedToken {
        line: format!("Type \"{}\"", tape_escape(token)),
        is_sleep: false,
    }
}

#[cfg(test)]
mod tests {
    use super::emit_token;

    #[test]
    fn sleep_token() {
        let result = emit_token("sleep:6");
        assert_eq!(result.line, "Sleep 6s");
        assert!(result.is_sleep);
    }

    #[test]
    fn text_token() {
        let result = emit_token("text:hello world");
        assert_eq!(result.line, "Type \"hello world\"");
        assert!(!result.is_sleep);
    }

    #[test]
    fn special_token() {
        let result = emit_token("tab");
        assert_eq!(result.line, "Tab");
        assert!(!result.is_sleep);
    }

    #[test]
    fn wait_alias_token_maps_to_sleep() {
        let result = emit_token("wait:500ms");
        assert_eq!(result.line, "Sleep 500ms");
        assert!(result.is_sleep);
    }

    #[test]
    fn text_token_escapes_quotes_and_backslashes() {
        let result = emit_token(r#"text:he"llo\world"#);
        assert_eq!(result.line, r#"Type "he\"llo\\world""#);
        assert!(!result.is_sleep);
    }

    #[test]
    fn single_character_token_is_typed_directly() {
        let result = emit_token("#");
        assert_eq!(result.line, "Type \"#\"");
        assert!(!result.is_sleep);
    }
}
