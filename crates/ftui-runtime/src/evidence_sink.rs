#![forbid(unsafe_code)]

//! JSONL evidence sink for deterministic diagnostics.
//!
//! This provides a shared, line-oriented sink that can be wired into runtime
//! policies (diff/resize/budget) to emit JSONL evidence to a single destination.
//! Ordering is deterministic with respect to call order because writes are
//! serialized behind a mutex, and flush behavior is explicit and configurable.
//!
//! ## Size cap
//!
//! File-backed sinks enforce a maximum size (default 50 MiB). Once the cap is
//! reached, further writes are silently dropped to prevent unbounded disk
//! growth. The cap can be configured via [`EvidenceSinkConfig::max_bytes`].

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Schema version for JSONL evidence lines.
pub const EVIDENCE_SCHEMA_VERSION: &str = "ftui-evidence-v1";

/// Default maximum evidence file size: 50 MiB.
pub const DEFAULT_MAX_EVIDENCE_BYTES: u64 = 50 * 1024 * 1024;

/// Destination for evidence JSONL output.
#[derive(Debug, Clone)]
pub enum EvidenceSinkDestination {
    /// Write to stdout.
    Stdout,
    /// Append to a file at the given path.
    File(PathBuf),
}

impl EvidenceSinkDestination {
    /// Convenience helper for file destinations.
    #[must_use]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }
}

/// Configuration for evidence logging.
#[derive(Debug, Clone)]
pub struct EvidenceSinkConfig {
    /// Whether evidence logging is enabled.
    pub enabled: bool,
    /// Output destination for JSONL lines.
    pub destination: EvidenceSinkDestination,
    /// Flush after every line (recommended for tests/e2e capture).
    pub flush_on_write: bool,
    /// Maximum total bytes to write before silently stopping.
    /// Only enforced for file destinations. `0` means unlimited.
    /// Defaults to [`DEFAULT_MAX_EVIDENCE_BYTES`] (50 MiB).
    pub max_bytes: u64,
}

impl Default for EvidenceSinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            destination: EvidenceSinkDestination::Stdout,
            flush_on_write: true,
            max_bytes: DEFAULT_MAX_EVIDENCE_BYTES,
        }
    }
}

impl EvidenceSinkConfig {
    /// Create a disabled sink config.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Enable logging to stdout with flush-on-write.
    #[must_use]
    pub fn enabled_stdout() -> Self {
        Self {
            enabled: true,
            destination: EvidenceSinkDestination::Stdout,
            flush_on_write: true,
            max_bytes: DEFAULT_MAX_EVIDENCE_BYTES,
        }
    }

    /// Enable logging to a file with flush-on-write.
    #[must_use]
    pub fn enabled_file(path: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            destination: EvidenceSinkDestination::file(path),
            flush_on_write: true,
            max_bytes: DEFAULT_MAX_EVIDENCE_BYTES,
        }
    }

    /// Set whether logging is enabled.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set the destination for evidence output.
    #[must_use]
    pub fn with_destination(mut self, destination: EvidenceSinkDestination) -> Self {
        self.destination = destination;
        self
    }

    /// Set flush-on-write behavior.
    #[must_use]
    pub fn with_flush_on_write(mut self, enabled: bool) -> Self {
        self.flush_on_write = enabled;
        self
    }

    /// Set maximum bytes before the sink silently stops writing.
    /// Use `0` for unlimited (not recommended for file destinations).
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

struct EvidenceSinkInner {
    writer: BufWriter<Box<dyn Write + Send>>,
    flush_on_write: bool,
    /// Maximum bytes allowed. `0` means unlimited.
    max_bytes: u64,
    /// Whether the size cap is enforced for this sink.
    cap_enabled: bool,
    /// Approximate total bytes written so far (including the initial file size).
    bytes_written: u64,
    /// Set to true once the cap is hit; prevents further writes.
    capped: bool,
}

/// Shared, line-oriented JSONL sink for evidence logging.
#[derive(Clone)]
pub struct EvidenceSink {
    inner: Arc<Mutex<EvidenceSinkInner>>,
}

impl std::fmt::Debug for EvidenceSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvidenceSink").finish()
    }
}

impl EvidenceSink {
    /// Build an evidence sink from config. Returns `Ok(None)` when disabled.
    ///
    /// For file destinations the existing file size is counted toward the cap
    /// so that restarting a process does not reset the budget. If the file
    /// already exceeds `max_bytes` the sink is returned in a "capped" state
    /// and no further bytes will be written.
    pub fn from_config(config: &EvidenceSinkConfig) -> io::Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }

        let (writer, existing_bytes): (Box<dyn Write + Send>, u64) = match &config.destination {
            EvidenceSinkDestination::Stdout => (Box::new(io::stdout()), 0),
            EvidenceSinkDestination::File(path) => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    fs::create_dir_all(parent)?;
                }
                let existing_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let file = OpenOptions::new().create(true).append(true).open(path)?;
                (Box::new(file), existing_size)
            }
        };

        let cap_enabled = matches!(&config.destination, EvidenceSinkDestination::File(_));
        let already_capped =
            cap_enabled && config.max_bytes > 0 && existing_bytes >= config.max_bytes;

        let inner = EvidenceSinkInner {
            writer: BufWriter::new(writer),
            flush_on_write: config.flush_on_write,
            max_bytes: config.max_bytes,
            cap_enabled,
            bytes_written: existing_bytes,
            capped: already_capped,
        };

        Ok(Some(Self {
            inner: Arc::new(Mutex::new(inner)),
        }))
    }

    /// Build an evidence sink that writes to an arbitrary `Write + Send` writer.
    #[must_use]
    pub fn from_writer(writer: impl Write + Send + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(EvidenceSinkInner {
                writer: BufWriter::new(Box::new(writer)),
                flush_on_write: true,
                max_bytes: 0,
                cap_enabled: false,
                bytes_written: 0,
                capped: false,
            })),
        }
    }

    /// Write a single JSONL line with newline and optional flush.
    ///
    /// If the file size cap has been reached, the write is silently dropped
    /// and `Ok(())` is returned so callers never see an error from capping.
    pub fn write_jsonl(&self, line: &str) -> io::Result<()> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        // Silently drop writes once the cap is hit.
        if inner.capped {
            return Ok(());
        }

        let line_bytes = u64::try_from(line.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1); // +1 for newline
        let new_total = inner.bytes_written.checked_add(line_bytes);

        // Check whether this write would exceed the cap.
        if inner.cap_enabled && inner.max_bytes > 0 && new_total.is_none_or(|n| n > inner.max_bytes)
        {
            inner.capped = true;
            // Best-effort: flush what we have so the file ends cleanly.
            let _ = inner.writer.flush();
            return Ok(());
        }

        inner.writer.write_all(line.as_bytes())?;
        inner.writer.write_all(b"\n")?;
        inner.bytes_written = new_total.unwrap_or(u64::MAX);
        if inner.flush_on_write {
            inner.writer.flush()?;
        }
        Ok(())
    }

    /// Flush any buffered output.
    pub fn flush(&self) -> io::Result<()> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_stable() {
        assert_eq!(EVIDENCE_SCHEMA_VERSION, "ftui-evidence-v1");
    }

    #[test]
    fn config_default_is_disabled() {
        let config = EvidenceSinkConfig::default();
        assert!(!config.enabled);
        assert!(config.flush_on_write);
        assert!(matches!(
            config.destination,
            EvidenceSinkDestination::Stdout
        ));
    }

    #[test]
    fn config_disabled_matches_default() {
        let config = EvidenceSinkConfig::disabled();
        assert!(!config.enabled);
    }

    #[test]
    fn config_enabled_stdout() {
        let config = EvidenceSinkConfig::enabled_stdout();
        assert!(config.enabled);
        assert!(config.flush_on_write);
        assert!(matches!(
            config.destination,
            EvidenceSinkDestination::Stdout
        ));
    }

    #[test]
    fn config_enabled_file() {
        let config = EvidenceSinkConfig::enabled_file("/tmp/test.jsonl");
        assert!(config.enabled);
        assert!(config.flush_on_write);
        assert!(matches!(
            config.destination,
            EvidenceSinkDestination::File(_)
        ));
    }

    #[test]
    fn config_builder_chain() {
        let config = EvidenceSinkConfig::default()
            .with_enabled(true)
            .with_destination(EvidenceSinkDestination::Stdout)
            .with_flush_on_write(false);
        assert!(config.enabled);
        assert!(!config.flush_on_write);
    }

    #[test]
    fn destination_file_helper() {
        let dest = EvidenceSinkDestination::file("/tmp/evidence.jsonl");
        assert!(
            matches!(dest, EvidenceSinkDestination::File(p) if p.to_str() == Some("/tmp/evidence.jsonl"))
        );
    }

    #[test]
    fn disabled_config_returns_none() {
        let config = EvidenceSinkConfig::disabled();
        let sink = EvidenceSink::from_config(&config).unwrap();
        assert!(sink.is_none());
    }

    #[test]
    fn enabled_file_sink_writes_jsonl() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempfile()
            .unwrap();
        let path = tmp.path().to_path_buf();
        let config = EvidenceSinkConfig::enabled_file(&path);
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();

        sink.write_jsonl(r#"{"event":"test","value":1}"#).unwrap();
        sink.write_jsonl(r#"{"event":"test","value":2}"#).unwrap();
        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], r#"{"event":"test","value":1}"#);
        assert_eq!(lines[1], r#"{"event":"test","value":2}"#);
    }

    #[test]
    fn sink_is_clone_and_shared() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempfile()
            .unwrap();
        let path = tmp.path().to_path_buf();
        let config = EvidenceSinkConfig::enabled_file(&path);
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();
        let sink2 = sink.clone();

        sink.write_jsonl(r#"{"from":"sink1"}"#).unwrap();
        sink2.write_jsonl(r#"{"from":"sink2"}"#).unwrap();
        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn sink_debug_impl() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempfile()
            .unwrap();
        let config = EvidenceSinkConfig::enabled_file(tmp.path());
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();
        let debug = format!("{:?}", sink);
        assert!(debug.contains("EvidenceSink"));
    }

    #[test]
    fn file_sink_caps_at_max_bytes() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempfile()
            .unwrap();
        let path = tmp.path().to_path_buf();
        // Set a very small cap: 100 bytes.
        let config = EvidenceSinkConfig::enabled_file(&path).with_max_bytes(100);
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();

        // Each line is ~30 bytes + newline. Write many times.
        for i in 0..100 {
            // Should never error, even after cap.
            sink.write_jsonl(&format!(r#"{{"event":"test","i":{i}}}"#))
                .unwrap();
        }
        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let size = content.len();
        assert!(
            size <= 100,
            "file should not exceed cap of 100 bytes, got {size}"
        );
        // At least one line should have been written.
        assert!(!content.is_empty(), "at least one line should be written");
    }

    #[test]
    fn file_sink_caps_on_preexisting_large_file() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempfile()
            .unwrap();
        let path = tmp.path().to_path_buf();
        // Pre-fill the file with 200 bytes.
        std::fs::write(&path, "x".repeat(200)).unwrap();

        let config = EvidenceSinkConfig::enabled_file(&path).with_max_bytes(100);
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();

        // This write should be silently dropped since file already exceeds cap.
        sink.write_jsonl(r#"{"event":"should_be_dropped"}"#)
            .unwrap();
        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("should_be_dropped"),
            "no new data should be written to an already-oversized file"
        );
    }

    #[test]
    fn file_sink_creates_parent_directories() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempdir()
            .unwrap();
        let path = tmp.path().join("nested").join("evidence.jsonl");
        let config = EvidenceSinkConfig::enabled_file(&path);
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();

        sink.write_jsonl(r#"{"event":"nested"}"#).unwrap();
        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"event\":\"nested\"}\n");
    }

    #[test]
    fn file_sink_caps_on_byte_counter_overflow() {
        let sink = EvidenceSink {
            inner: Arc::new(Mutex::new(EvidenceSinkInner {
                writer: BufWriter::new(Box::new(io::sink())),
                flush_on_write: true,
                max_bytes: u64::MAX,
                cap_enabled: true,
                bytes_written: u64::MAX - 1,
                capped: false,
            })),
        };

        sink.write_jsonl("{}").unwrap();

        let inner = sink.inner.lock().unwrap();
        assert!(
            inner.capped,
            "overflowing cap accounting should cap the sink"
        );
        assert_eq!(inner.bytes_written, u64::MAX - 1);
    }

    #[test]
    fn unlimited_max_bytes_allows_unbounded_writes() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-evidence-sink-")
            .disable_cleanup(true)
            .tempfile()
            .unwrap();
        let path = tmp.path().to_path_buf();
        let config = EvidenceSinkConfig::enabled_file(&path).with_max_bytes(0);
        let sink = EvidenceSink::from_config(&config).unwrap().unwrap();

        for i in 0..1000 {
            sink.write_jsonl(&format!(r#"{{"i":{i}}}"#)).unwrap();
        }
        sink.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1000, "all 1000 lines should be written");
    }

    #[test]
    fn default_max_bytes_is_50mib() {
        let config = EvidenceSinkConfig::default();
        assert_eq!(config.max_bytes, DEFAULT_MAX_EVIDENCE_BYTES);
        assert_eq!(config.max_bytes, 50 * 1024 * 1024);
    }
}
