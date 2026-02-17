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
