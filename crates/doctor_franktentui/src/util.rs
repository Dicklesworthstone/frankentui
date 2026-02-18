use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Local, Utc};
use fastapi_output::RichOutput;
use serde::Serialize;
use sqlmodel_console::OutputMode as SqlModelOutputMode;

use crate::error::{DoctorError, Result};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[must_use]
pub fn now_utc_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[must_use]
pub fn now_compact_timestamp() -> String {
    Local::now().format("%Y%m%d_%H%M%S").to_string()
}

pub fn command_exists(command: &str) -> bool {
    which::which(command).is_ok()
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputIntegration {
    pub fastapi_mode: String,
    pub fastapi_agent: bool,
    pub fastapi_ci: bool,
    pub fastapi_tty: bool,
    pub sqlmodel_mode: String,
    pub sqlmodel_agent: bool,
}

impl OutputIntegration {
    #[must_use]
    pub fn detect() -> Self {
        let fastapi_detection = fastapi_output::detect_environment();
        let fastapi_mode = fastapi_output::OutputMode::auto();
        let sqlmodel_mode = SqlModelOutputMode::detect();
        Self {
            fastapi_mode: fastapi_mode.as_str().to_string(),
            fastapi_agent: fastapi_detection.is_agent,
            fastapi_ci: fastapi_detection.is_ci,
            fastapi_tty: fastapi_detection.is_tty,
            sqlmodel_mode: sqlmodel_mode.as_str().to_string(),
            sqlmodel_agent: SqlModelOutputMode::is_agent_environment(),
        }
    }

    #[must_use]
    pub fn should_emit_json(&self) -> bool {
        self.sqlmodel_mode == "json"
    }

    #[must_use]
    pub fn as_json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct CliOutput {
    inner: RichOutput,
    enabled: bool,
}

impl CliOutput {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: RichOutput::auto(),
            enabled,
        }
    }

    pub fn rule(&self, title: Option<&str>) {
        if self.enabled {
            self.inner.rule(title);
        }
    }

    pub fn info(&self, message: &str) {
        if self.enabled {
            self.inner.info(message);
        }
    }

    pub fn success(&self, message: &str) {
        if self.enabled {
            self.inner.success(message);
        }
    }

    pub fn warning(&self, message: &str) {
        if self.enabled {
            self.inner.warning(message);
        }
    }

    pub fn error(&self, message: &str) {
        if self.enabled {
            self.inner.error(message);
        }
    }
}

#[must_use]
pub fn output_for(integration: &OutputIntegration) -> CliOutput {
    CliOutput::new(!integration.should_emit_json())
}

#[must_use]
pub fn output() -> RichOutput {
    RichOutput::auto()
}

pub fn require_command(command: &str) -> Result<()> {
    if command_exists(command) {
        Ok(())
    } else {
        Err(DoctorError::MissingCommand {
            command: command.to_string(),
        })
    }
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

pub fn ensure_exists(path: &Path) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        Err(DoctorError::MissingPath {
            path: path.to_path_buf(),
        })
    }
}

pub fn ensure_executable(path: &Path) -> Result<()> {
    ensure_exists(path)?;

    #[cfg(unix)]
    {
        let metadata = fs::metadata(path)?;
        let mode = metadata.permissions().mode();
        if mode & 0o111 != 0 {
            return Ok(());
        }
        Err(DoctorError::NotExecutable {
            path: path.to_path_buf(),
        })
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub fn write_string(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

pub fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[must_use]
pub fn bool_to_u8(value: bool) -> u8 {
    u8::from(value)
}

pub fn parse_duration_value(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DoctorError::invalid("duration value cannot be empty"));
    }

    if let Some(ms) = trimmed.strip_suffix("ms") {
        let value = ms
            .trim()
            .parse::<u64>()
            .map_err(|_| DoctorError::invalid(format!("invalid millisecond duration: {raw}")))?;
        return Ok(Duration::from_millis(value));
    }

    if let Some(sec) = trimmed.strip_suffix('s') {
        let value = sec
            .trim()
            .parse::<u64>()
            .map_err(|_| DoctorError::invalid(format!("invalid second duration: {raw}")))?;
        return Ok(Duration::from_secs(value));
    }

    let value = trimmed
        .parse::<u64>()
        .map_err(|_| DoctorError::invalid(format!("invalid duration value: {raw}")))?;
    Ok(Duration::from_secs(value))
}

#[must_use]
pub fn normalize_http_path(path: &str) -> String {
    let mut value = path.trim().to_string();
    if !value.starts_with('/') {
        value.insert(0, '/');
    }
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

#[must_use]
pub fn shell_single_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[must_use]
pub fn duration_literal(value: &str) -> String {
    let has_alpha = value.chars().any(char::is_alphabetic);
    if has_alpha {
        value.to_string()
    } else {
        format!("{value}s")
    }
}

#[must_use]
pub fn tape_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[must_use]
pub fn relative_to(base: &Path, path: &Path) -> Option<PathBuf> {
    pathdiff::diff_paths(path, base)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{
        OutputIntegration, duration_literal, normalize_http_path, output_for, parse_duration_value,
        relative_to, shell_single_quote, tape_escape,
    };

    #[test]
    fn parse_duration_supports_ms_s_and_plain_seconds() {
        assert_eq!(
            parse_duration_value("250ms").expect("ms duration"),
            Duration::from_millis(250)
        );
        assert_eq!(
            parse_duration_value("7s").expect("seconds duration"),
            Duration::from_secs(7)
        );
        assert_eq!(
            parse_duration_value("9").expect("plain seconds duration"),
            Duration::from_secs(9)
        );
    }

    #[test]
    fn parse_duration_rejects_invalid_values() {
        let empty = parse_duration_value("").expect_err("empty duration should fail");
        assert!(empty.to_string().contains("duration value cannot be empty"));

        let malformed = parse_duration_value("bad").expect_err("malformed duration should fail");
        assert!(malformed.to_string().contains("invalid duration value"));
    }

    #[test]
    fn normalize_http_path_enforces_boundaries() {
        assert_eq!(normalize_http_path("mcp"), "/mcp/");
        assert_eq!(normalize_http_path("/mcp"), "/mcp/");
        assert_eq!(normalize_http_path("/mcp/"), "/mcp/");
        assert_eq!(normalize_http_path("  custom/path "), "/custom/path/");
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quote() {
        let escaped = shell_single_quote("a'b");
        assert_eq!(escaped, "'a'\"'\"'b'");
    }

    #[test]
    fn duration_literal_appends_seconds_only_when_missing_units() {
        assert_eq!(duration_literal("5"), "5s");
        assert_eq!(duration_literal("500ms"), "500ms");
    }

    #[test]
    fn tape_escape_escapes_quotes_and_backslashes() {
        let escaped = tape_escape("a\\b\"c");
        assert_eq!(escaped, "a\\\\b\\\"c");
    }

    #[test]
    fn relative_to_returns_path_relative_to_base() {
        let base = Path::new("/tmp/root");
        let target = Path::new("/tmp/root/a/b.txt");
        let relative = relative_to(base, target).expect("relative path");
        assert_eq!(relative, Path::new("a/b.txt"));
    }

    #[test]
    fn output_for_disables_human_output_when_json_mode_requested() {
        let json_integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: true,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: true,
        };
        let human_integration = OutputIntegration {
            sqlmodel_mode: "plain".to_string(),
            ..json_integration.clone()
        };

        let json_output = output_for(&json_integration);
        let human_output = output_for(&human_integration);

        assert!(!json_output.enabled);
        assert!(human_output.enabled);
    }
}
