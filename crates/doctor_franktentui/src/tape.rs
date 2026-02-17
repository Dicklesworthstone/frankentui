use std::path::Path;

use crate::keyseq::emit_token;
use crate::util::{duration_literal, tape_escape};

#[derive(Debug, Clone)]
pub struct TapeSpec<'a> {
    pub output: &'a Path,
    pub required_binary: Option<&'a Path>,
    pub project_dir: &'a Path,
    pub server_command: &'a str,
    pub font_size: u16,
    pub width: u16,
    pub height: u16,
    pub framerate: u16,
    pub theme: &'a str,
    pub boot_sleep: &'a str,
    pub step_sleep: &'a str,
    pub tail_sleep: &'a str,
    pub keys: &'a str,
}

#[must_use]
pub fn build_capture_tape(spec: &TapeSpec<'_>) -> String {
    let mut lines = vec![
        format!("Output \"{}\"", spec.output.display()),
        String::new(),
        "Set Shell \"bash\"".to_string(),
        format!("Set FontSize {}", spec.font_size),
        format!("Set Width {}", spec.width),
        format!("Set Height {}", spec.height),
        format!("Set Framerate {}", spec.framerate),
        "Set TypingSpeed 0ms".to_string(),
        format!("Set Theme \"{}\"", tape_escape(spec.theme)),
        String::new(),
        "Hide".to_string(),
        format!(
            "Type \"{}\"",
            tape_escape(&format!("cd {}", spec.project_dir.display()))
        ),
        "Enter".to_string(),
        format!("Type \"{}\"", tape_escape(spec.server_command)),
        "Enter".to_string(),
        "Show".to_string(),
        String::new(),
        format!("Sleep {}", duration_literal(spec.boot_sleep)),
    ];

    if let Some(binary) = spec.required_binary {
        lines.insert(2, format!("Require \"{}\"", binary.display()));
        lines.insert(3, String::new());
    }

    for raw_token in spec.keys.split(',') {
        let emitted = emit_token(raw_token);
        lines.push(emitted.line);
        if !emitted.is_sleep {
            lines.push(format!("Sleep {}", duration_literal(spec.step_sleep)));
        }
    }

    lines.push(format!("Sleep {}", duration_literal(spec.tail_sleep)));
    lines.push("Ctrl+C".to_string());
    lines.push("Sleep 500ms".to_string());
    lines.push(String::new());

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{TapeSpec, build_capture_tape};

    #[test]
    fn tape_contains_required_sections() {
        let spec = TapeSpec {
            output: Path::new("/tmp/out.mp4"),
            required_binary: Some(Path::new("/tmp/bin")),
            project_dir: Path::new("/tmp/project"),
            server_command: "echo run",
            font_size: 20,
            width: 1600,
            height: 900,
            framerate: 30,
            theme: "GruvboxDark",
            boot_sleep: "6",
            step_sleep: "1",
            tail_sleep: "1",
            keys: "#,sleep:2,q",
        };

        let tape = build_capture_tape(&spec);
        assert!(tape.contains("Require \"/tmp/bin\""));
        assert!(tape.contains("Set Theme \"GruvboxDark\""));
        assert!(tape.contains("Type \"#\""));
        assert!(tape.contains("Sleep 2s"));
        assert!(tape.contains("Ctrl+C"));
    }
}
