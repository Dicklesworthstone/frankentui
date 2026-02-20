use std::path::Path;

use crate::keyseq::emit_token;
use crate::util::{duration_literal, shell_single_quote, tape_escape};

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
    let output_literal = tape_escape(&spec.output.display().to_string());
    let project_dir_literal = shell_single_quote(&spec.project_dir.display().to_string());

    let mut lines = vec![
        format!("Output \"{output_literal}\""),
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
            tape_escape(&format!("cd {project_dir_literal}"))
        ),
        "Enter".to_string(),
        "Type \"export PS1=''\"".to_string(),
        "Enter".to_string(),
        "Type \"clear\"".to_string(),
        "Enter".to_string(),
        format!(
            "Type \"{}\"",
            tape_escape(&format!("exec {}", spec.server_command))
        ),
        "Enter".to_string(),
        "Show".to_string(),
        String::new(),
        format!("Sleep {}", duration_literal(spec.boot_sleep)),
    ];

    if let Some(binary) = spec.required_binary {
        lines.insert(
            2,
            format!("Require \"{}\"", tape_escape(&binary.display().to_string())),
        );
        lines.insert(3, String::new());
    }

    for raw_token in spec.keys.split(',') {
        if raw_token.trim().is_empty() {
            continue;
        }
        let emitted = emit_token(raw_token);
        lines.push(emitted.line);
        if !emitted.is_sleep {
            lines.push(format!("Sleep {}", duration_literal(spec.step_sleep)));
        }
    }

    lines.push(format!("Sleep {}", duration_literal(spec.tail_sleep)));
    // Hide teardown to avoid recording shell prompt/command artifacts in output frames.
    lines.push("Hide".to_string());
    // Robust shutdown tail:
    // - first Ctrl+C handles normal SIGINT shutdown
    // - second Ctrl+C handles apps with confirmation-on-interrupt
    lines.push("Ctrl+C".to_string());
    lines.push("Sleep 500ms".to_string());
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
        assert!(tape.contains("Type \"export PS1=''\""));
        assert!(tape.contains("Type \"clear\""));
        assert!(tape.contains("Type \"exec echo run\""));
        assert!(tape.contains("Type \"#\""));
        assert!(tape.contains("Sleep 2s"));
        assert_eq!(tape.matches("\nHide\n").count(), 2);
        assert!(tape.contains("\nHide\nCtrl+C\n"));
        assert_eq!(tape.matches("Ctrl+C").count(), 2);
        assert!(!tape.contains("Type \"exit\""));
    }

    #[test]
    fn tape_omits_require_when_no_binary_is_needed() {
        let spec = TapeSpec {
            output: Path::new("/tmp/out.mp4"),
            required_binary: None,
            project_dir: Path::new("/tmp/project"),
            server_command: "echo run",
            font_size: 20,
            width: 1600,
            height: 900,
            framerate: 30,
            theme: "Light",
            boot_sleep: "1",
            step_sleep: "1",
            tail_sleep: "1",
            keys: "a,q",
        };

        let tape = build_capture_tape(&spec);
        assert!(!tape.contains("Require "));
    }

    #[test]
    fn tape_inserts_step_sleep_only_after_non_sleep_tokens() {
        let spec = TapeSpec {
            output: Path::new("/tmp/out.mp4"),
            required_binary: None,
            project_dir: Path::new("/tmp/project"),
            server_command: "echo run",
            font_size: 20,
            width: 1600,
            height: 900,
            framerate: 30,
            theme: "Theme",
            boot_sleep: "7",
            step_sleep: "9",
            tail_sleep: "11",
            keys: "a,sleep:2,b",
        };

        let tape = build_capture_tape(&spec);
        let step_sleep_count = tape.matches("Sleep 9s").count();
        assert_eq!(step_sleep_count, 2);
        assert!(tape.contains("Sleep 2s"));
        assert!(tape.contains("Sleep 11s"));
    }

    #[test]
    fn tape_quotes_project_dir_and_escapes_output_and_require_paths() {
        let spec = TapeSpec {
            output: Path::new("/tmp/out \"quoted\".mp4"),
            required_binary: Some(Path::new("/tmp/bin \"quoted\"")),
            project_dir: Path::new("/tmp/project dir"),
            server_command: "echo run",
            font_size: 20,
            width: 1600,
            height: 900,
            framerate: 30,
            theme: "Theme",
            boot_sleep: "1",
            step_sleep: "1",
            tail_sleep: "1",
            keys: "q",
        };

        let tape = build_capture_tape(&spec);
        assert!(tape.contains("Output \"/tmp/out \\\"quoted\\\".mp4\""));
        assert!(tape.contains("Require \"/tmp/bin \\\"quoted\\\"\""));
        assert!(tape.contains("Type \"cd '/tmp/project dir'\""));
    }

    #[test]
    fn tape_skips_empty_tokens_in_key_sequence() {
        let spec = TapeSpec {
            output: Path::new("/tmp/out.mp4"),
            required_binary: None,
            project_dir: Path::new("/tmp/project"),
            server_command: "echo run",
            font_size: 20,
            width: 1600,
            height: 900,
            framerate: 30,
            theme: "Theme",
            boot_sleep: "1",
            step_sleep: "3",
            tail_sleep: "1",
            keys: "a,,  ,sleep:1,,b,",
        };

        let tape = build_capture_tape(&spec);
        assert!(!tape.contains("Type \"\""));
        assert_eq!(tape.matches("Sleep 3s").count(), 2);
        assert!(tape.contains("Sleep 1s"));
    }
}
