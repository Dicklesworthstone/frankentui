use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use chrono::{Local, Utc};
use fastapi_output::RichOutput;
use serde::Serialize;
use sqlmodel_console::OutputMode as SqlModelOutputMode;

use crate::error::{DoctorError, Result};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};

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

#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputModeOverride {
    Human,
    Json,
}

const OUTPUT_MODE_OVERRIDE_AUTO: u8 = 0;
const OUTPUT_MODE_OVERRIDE_HUMAN: u8 = 1;
const OUTPUT_MODE_OVERRIDE_JSON: u8 = 2;
static OUTPUT_MODE_OVERRIDE: AtomicU8 = AtomicU8::new(OUTPUT_MODE_OVERRIDE_AUTO);

fn current_output_mode_override() -> Option<OutputModeOverride> {
    match OUTPUT_MODE_OVERRIDE.load(Ordering::Relaxed) {
        OUTPUT_MODE_OVERRIDE_HUMAN => Some(OutputModeOverride::Human),
        OUTPUT_MODE_OVERRIDE_JSON => Some(OutputModeOverride::Json),
        _ => None,
    }
}

pub fn set_output_mode_override(mode: Option<OutputModeOverride>) {
    let encoded = match mode {
        Some(OutputModeOverride::Human) => OUTPUT_MODE_OVERRIDE_HUMAN,
        Some(OutputModeOverride::Json) => OUTPUT_MODE_OVERRIDE_JSON,
        None => OUTPUT_MODE_OVERRIDE_AUTO,
    };
    OUTPUT_MODE_OVERRIDE.store(encoded, Ordering::Relaxed);
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
        let override_mode = current_output_mode_override();
        let fastapi_mode = match override_mode {
            Some(OutputModeOverride::Human) => "plain".to_string(),
            Some(OutputModeOverride::Json) => "json".to_string(),
            None => fastapi_output::OutputMode::auto().as_str().to_string(),
        };
        let sqlmodel_mode = match override_mode {
            Some(OutputModeOverride::Human) => "plain".to_string(),
            Some(OutputModeOverride::Json) => "json".to_string(),
            None => SqlModelOutputMode::detect().as_str().to_string(),
        };
        Self {
            fastapi_mode,
            fastapi_agent: fastapi_detection.is_agent,
            fastapi_ci: fastapi_detection.is_ci,
            fastapi_tty: fastapi_detection.is_tty,
            sqlmodel_mode,
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

pub fn ensure_safe_path_component(value: &str, field_name: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) if name == OsStr::new(value) => Ok(()),
        _ => Err(DoctorError::invalid(format!(
            "{field_name} must be a single safe path component: {value}",
        ))),
    }
}

pub fn join_validated_child_path(
    base: &Path,
    child_name: &str,
    field_name: &str,
) -> Result<PathBuf> {
    ensure_safe_path_component(child_name, field_name)?;
    Ok(base.join(child_name))
}

fn invalid_snapshot_data_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn copy_tree_snapshot_materialized<F>(
    source_dir: &Path,
    snapshot_dir: &Path,
    should_skip: F,
) -> io::Result<()>
where
    F: Fn(&Path) -> bool,
{
    let canonical_root = fs::canonicalize(source_dir).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "unable to resolve source root {}: {error}",
                source_dir.display()
            ),
        )
    })?;

    let mut active_dirs = BTreeSet::new();
    copy_tree_snapshot_materialized_inner(
        source_dir,
        snapshot_dir,
        Path::new(""),
        &canonical_root,
        &should_skip,
        &mut active_dirs,
    )
}

fn copy_tree_snapshot_materialized_inner<F>(
    source_dir: &Path,
    snapshot_dir: &Path,
    relative_prefix: &Path,
    canonical_root: &Path,
    should_skip: &F,
    active_dirs: &mut BTreeSet<PathBuf>,
) -> io::Result<()>
where
    F: Fn(&Path) -> bool,
{
    let canonical_dir = fs::canonicalize(source_dir).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "unable to resolve source directory {}: {error}",
                source_dir.display()
            ),
        )
    })?;

    if !canonical_dir.starts_with(canonical_root) {
        return Err(invalid_snapshot_data_error(format!(
            "snapshot source escapes source root: {}",
            source_dir.display()
        )));
    }

    if !active_dirs.insert(canonical_dir.clone()) {
        return Err(invalid_snapshot_data_error(format!(
            "symlink cycle detected while materializing snapshot at {}",
            source_dir.display()
        )));
    }

    fs::create_dir_all(snapshot_dir)?;

    let result = (|| -> io::Result<()> {
        for entry in fs::read_dir(source_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let relative_path = if relative_prefix.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                relative_prefix.join(&name)
            };

            if should_skip(&relative_path) {
                continue;
            }

            let source_path = entry.path();
            let target_path = snapshot_dir.join(&name);
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                copy_tree_snapshot_materialized_inner(
                    &source_path,
                    &target_path,
                    &relative_path,
                    canonical_root,
                    should_skip,
                    active_dirs,
                )?;
                continue;
            }

            if file_type.is_file() {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&source_path, &target_path)?;
                continue;
            }

            if file_type.is_symlink() {
                materialize_snapshot_symlink(
                    &source_path,
                    &target_path,
                    &relative_path,
                    canonical_root,
                    should_skip,
                    active_dirs,
                )?;
                continue;
            }

            return Err(invalid_snapshot_data_error(format!(
                "unsupported filesystem entry in snapshot source: {}",
                source_path.display()
            )));
        }

        Ok(())
    })();

    active_dirs.remove(&canonical_dir);
    result
}

fn materialize_snapshot_symlink<F>(
    source_path: &Path,
    target_path: &Path,
    relative_path: &Path,
    canonical_root: &Path,
    should_skip: &F,
    active_dirs: &mut BTreeSet<PathBuf>,
) -> io::Result<()>
where
    F: Fn(&Path) -> bool,
{
    let resolved_path = fs::canonicalize(source_path).map_err(|error| {
        invalid_snapshot_data_error(format!(
            "unable to resolve symlink {}: {error}",
            source_path.display()
        ))
    })?;

    if !resolved_path.starts_with(canonical_root) {
        return Err(invalid_snapshot_data_error(format!(
            "snapshot source symlink escapes source root: {} -> {}",
            source_path.display(),
            resolved_path.display()
        )));
    }

    if let Ok(resolved_relative) = resolved_path.strip_prefix(canonical_root)
        && should_skip(resolved_relative)
    {
        return Ok(());
    }

    let metadata = fs::metadata(source_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "unable to inspect symlink target {}: {error}",
                source_path.display()
            ),
        )
    })?;

    if metadata.is_dir() {
        return copy_tree_snapshot_materialized_inner(
            &resolved_path,
            target_path,
            relative_path,
            canonical_root,
            should_skip,
            active_dirs,
        );
    }

    if metadata.is_file() {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&resolved_path, target_path)?;
        return Ok(());
    }

    Err(invalid_snapshot_data_error(format!(
        "snapshot source symlink points to unsupported entry type: {}",
        source_path.display()
    )))
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
pub fn exit_status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return 128 + signal;
    }

    1
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
pub fn tmux_attach_command_literal(session_name: &str) -> String {
    format!(
        "tmux attach-session -t {}",
        shell_single_quote(session_name)
    )
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
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tempfile::tempdir;

    use crate::error::DoctorError;

    use super::{
        OutputIntegration, append_line, bool_to_u8, command_exists,
        copy_tree_snapshot_materialized, duration_literal, ensure_dir, ensure_executable,
        ensure_exists, ensure_safe_path_component, exit_status_code, join_validated_child_path,
        normalize_http_path, output_for, parse_duration_value, relative_to, require_command,
        shell_single_quote, tape_escape, tmux_attach_command_literal, write_string,
    };

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

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

    #[cfg(unix)]
    #[test]
    fn exit_status_code_preserves_signal_exit_using_shell_convention() {
        let status = std::process::ExitStatus::from_raw(15);
        assert_eq!(exit_status_code(status), 143);
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
    fn tmux_attach_command_literal_quotes_session_name_for_shell() {
        assert_eq!(
            tmux_attach_command_literal("session name"),
            "tmux attach-session -t 'session name'"
        );
        assert_eq!(
            tmux_attach_command_literal("a'b"),
            "tmux attach-session -t 'a'\"'\"'b'"
        );
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

    #[test]
    fn output_integration_as_json_line_round_trips() {
        let integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: true,
            fastapi_ci: false,
            fastapi_tty: true,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: true,
        };

        let line = integration.as_json_line();
        let parsed = serde_json::from_str::<serde_json::Value>(&line).expect("as_json_line JSON");

        assert_eq!(parsed["sqlmodel_mode"], "json");
        assert_eq!(parsed["fastapi_tty"], true);
    }

    #[test]
    fn bool_to_u8_converts_values() {
        assert_eq!(bool_to_u8(false), 0);
        assert_eq!(bool_to_u8(true), 1);
    }

    #[test]
    fn ensure_dir_creates_nested_directory() {
        let temp = tempdir().expect("tempdir");
        let nested = temp.path().join("a/b/c");
        ensure_dir(&nested).expect("ensure_dir");
        assert!(nested.is_dir());
    }

    #[test]
    fn ensure_exists_reports_missing_path_error() {
        let temp = tempdir().expect("tempdir");
        let missing = temp.path().join("does-not-exist");
        let error = ensure_exists(&missing).expect_err("missing path should error");
        assert!(matches!(error, DoctorError::MissingPath { path } if path == missing));
    }

    #[test]
    fn write_string_creates_parent_dirs_and_writes_content() {
        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("nested/dir/file.txt");
        write_string(&target, "hello").expect("write_string");
        let content = std::fs::read_to_string(&target).expect("read file");
        assert_eq!(content, "hello");
    }

    #[test]
    fn append_line_creates_parent_dirs_and_appends_newlines() {
        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("logs/out.txt");

        append_line(&target, "first").expect("append first");
        append_line(&target, "second").expect("append second");

        let content = std::fs::read_to_string(&target).expect("read file");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn command_exists_and_require_command_agree_on_missing_binary() {
        let missing = "definitely-not-a-real-doctor-frankentui-command";
        assert!(!command_exists(missing));

        let error = require_command(missing).expect_err("missing command should fail");
        assert!(matches!(error, DoctorError::MissingCommand { command } if command == missing));
    }

    #[test]
    fn ensure_executable_reports_missing_path_error() {
        let missing = PathBuf::from("/tmp/doctor_frankentui/missing-executable");
        let error = ensure_executable(&missing).expect_err("missing executable should fail");
        assert!(matches!(error, DoctorError::MissingPath { path } if path == missing));
    }

    #[test]
    fn join_validated_child_path_accepts_single_component() {
        let base = Path::new("/tmp/root");
        let joined = join_validated_child_path(base, "child_dir", "run_name")
            .expect("safe child path should validate");
        assert_eq!(joined, base.join("child_dir"));
    }

    #[test]
    fn ensure_safe_path_component_rejects_traversal() {
        let error = ensure_safe_path_component("../escape", "run_name")
            .expect_err("traversal should be rejected");
        assert!(
            error
                .to_string()
                .contains("run_name must be a single safe path component")
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_snapshot_materialized_materializes_internal_file_symlinks() {
        use std::os::unix::fs::symlink;

        let src = tempdir().expect("tempdir");
        let dst = tempdir().expect("tempdir");

        write_string(&src.path().join("real.txt"), "hello").expect("write real file");
        symlink(src.path().join("real.txt"), src.path().join("alias.txt"))
            .expect("create internal symlink");

        copy_tree_snapshot_materialized(src.path(), dst.path(), |_| false)
            .expect("copy snapshot with internal symlink");

        let alias_path = dst.path().join("alias.txt");
        assert_eq!(
            std::fs::read_to_string(&alias_path).expect("read alias"),
            "hello"
        );
        assert!(
            !std::fs::symlink_metadata(&alias_path)
                .expect("metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_snapshot_materialized_rejects_symlink_escape() {
        use std::io::ErrorKind;
        use std::os::unix::fs::symlink;

        let src = tempdir().expect("tempdir");
        let dst = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");

        write_string(&outside.path().join("secret.txt"), "secret").expect("write outside file");
        symlink(
            outside.path().join("secret.txt"),
            src.path().join("escape.txt"),
        )
        .expect("create escape symlink");

        let error = copy_tree_snapshot_materialized(src.path(), dst.path(), |_| false)
            .expect_err("symlink escape should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_executable_rejects_non_exec_file_and_accepts_exec_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("script.sh");
        // doctor_frankentui:no-fake-allow (unit test) writes a temp shell script to
        // validate unix executable-bit handling (real filesystem permissions, no binary shims).
        write_string(&target, "#!/bin/sh\necho hi\n").expect("write script");

        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .expect("chmod 644");

        let error = ensure_executable(&target).expect_err("non-exec file should fail");
        assert!(matches!(error, DoctorError::NotExecutable { path } if path == target));

        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("chmod 755");
        ensure_executable(&target).expect("exec file should pass");

        let explicit = target.display().to_string();
        assert!(command_exists(&explicit));
        require_command(&explicit).expect("require_command should accept explicit executable path");
    }
}
