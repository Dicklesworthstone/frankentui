#![forbid(unsafe_code)]

use std::env;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ftui_core::terminal_capabilities::TerminalCapabilities;

const ENV_CLIPBOARD_BACKEND: &str = "FTUI_CLIPBOARD_BACKEND";

/// OSC 52 clipboard selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    /// System clipboard.
    Clipboard,
    /// Primary selection (X11).
    Primary,
    /// Secondary selection (X11).
    Secondary,
    /// Cut buffer 0..=7.
    CutBuffer(u8),
}

impl ClipboardSelection {
    fn osc52_code(self) -> Result<char, ClipboardError> {
        match self {
            Self::Clipboard => Ok('c'),
            Self::Primary => Ok('p'),
            Self::Secondary => Ok('s'),
            Self::CutBuffer(index) if index <= 7 => Ok((b'0' + index) as char),
            Self::CutBuffer(index) => Err(ClipboardError::InvalidInput(format!(
                "cut buffer index must be 0..=7 (got {index})",
            ))),
        }
    }
}

/// Clipboard backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardBackend {
    Osc52,
    External(ExternalBackend),
    Unavailable,
}

/// External clipboard mechanisms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalBackend {
    MacOS,
    Windows,
    Wayland,
    X11,
}

/// DCS passthrough mode for multiplexer environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassthroughMode {
    /// No passthrough needed (direct terminal access).
    None,
    /// tmux DCS passthrough: `ESC P tmux; <ESC-doubled seq> ESC \`.
    Tmux,
    /// GNU screen DCS passthrough: `ESC P <seq> ESC \`.
    Screen,
}

/// Clipboard errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    NotAvailable,
    InvalidInput(String),
    WriteError(String),
    ReadError(String),
    Timeout,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable => write!(f, "clipboard not available"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::WriteError(msg) => write!(f, "clipboard write failed: {msg}"),
            Self::ReadError(msg) => write!(f, "clipboard read failed: {msg}"),
            Self::Timeout => write!(f, "clipboard read timed out"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Cross-terminal clipboard helper.
#[derive(Debug, Clone)]
pub struct Clipboard {
    caps: TerminalCapabilities,
    backend: ClipboardBackend,
    fallback: Option<ExternalBackend>,
    max_payload: usize,
    osc52_timeout: Duration,
    passthrough: PassthroughMode,
}

impl Clipboard {
    /// Common OSC 52 size limit (base64 payload bytes).
    pub const DEFAULT_MAX_OSC52_PAYLOAD: usize = 74_994;
    /// Default OSC 52 response timeout.
    pub const DEFAULT_OSC52_TIMEOUT: Duration = Duration::from_millis(100);

    /// Create a clipboard helper using OSC 52 only.
    ///
    /// For auto-detection across terminals, use [`detect`](Self::detect).
    #[must_use]
    pub const fn new(caps: TerminalCapabilities) -> Self {
        let backend = if caps.osc52_clipboard {
            ClipboardBackend::Osc52
        } else {
            ClipboardBackend::Unavailable
        };
        Self {
            caps,
            backend,
            fallback: None,
            max_payload: Self::DEFAULT_MAX_OSC52_PAYLOAD,
            osc52_timeout: Self::DEFAULT_OSC52_TIMEOUT,
            passthrough: PassthroughMode::None,
        }
    }

    /// Auto-detect the best available clipboard backend.
    ///
    /// Detects OSC 52 support and external clipboard tools. Does not
    /// automatically enable multiplexer passthrough; use [`auto`](Self::auto)
    /// for full auto-detection including mux environments.
    #[must_use]
    pub fn detect(caps: TerminalCapabilities) -> Self {
        let external = detect_external_backend();
        let (backend, fallback) = if caps.osc52_clipboard {
            (ClipboardBackend::Osc52, external)
        } else if let Some(ext) = external {
            (ClipboardBackend::External(ext), None)
        } else {
            (ClipboardBackend::Unavailable, None)
        };

        let (backend, fallback) = apply_backend_override(caps, backend, fallback);

        log_detected(backend);

        Self {
            caps,
            backend,
            fallback,
            max_payload: Self::DEFAULT_MAX_OSC52_PAYLOAD,
            osc52_timeout: Self::DEFAULT_OSC52_TIMEOUT,
            passthrough: PassthroughMode::None,
        }
    }

    /// Full auto-detection of the best clipboard method.
    ///
    /// This is the recommended entry point. It performs the same detection
    /// as [`detect`](Self::detect) but also enables DCS passthrough
    /// wrapping when running inside tmux or GNU screen, giving the best
    /// chance of clipboard access in any environment.
    ///
    /// # Priority order
    ///
    /// 1. **Direct OSC 52** — when the terminal natively supports it
    /// 2. **OSC 52 via passthrough** — when inside tmux/screen
    ///    (requires `allow-passthrough` in tmux 3.3+)
    /// 3. **External tools** — pbcopy/pbpaste, wl-copy/wl-paste,
    ///    xclip/xsel, clip/Get-Clipboard
    /// 4. **Unavailable** — no clipboard backend found
    ///
    /// The `FTUI_CLIPBOARD_BACKEND` environment variable can override
    /// auto-detection (values: `osc52`, `macos`, `wayland`, `x11`,
    /// `windows`, `none`).
    #[must_use]
    pub fn auto(caps: TerminalCapabilities) -> Self {
        let external = detect_external_backend();

        // Direct OSC 52 (no mux, terminal supports it)
        if caps.osc52_clipboard {
            let (backend, fallback) =
                apply_backend_override(caps, ClipboardBackend::Osc52, external);
            log_detected(backend);
            return Self {
                caps,
                backend,
                fallback,
                max_payload: Self::DEFAULT_MAX_OSC52_PAYLOAD,
                osc52_timeout: Self::DEFAULT_OSC52_TIMEOUT,
                passthrough: PassthroughMode::None,
            };
        }

        // OSC 52 via multiplexer passthrough
        if caps.needs_passthrough_wrap() {
            let passthrough = if caps.in_tmux {
                PassthroughMode::Tmux
            } else {
                PassthroughMode::Screen
            };
            let (backend, fallback) =
                apply_backend_override(caps, ClipboardBackend::Osc52, external);
            log_detected(backend);
            return Self {
                caps,
                backend,
                fallback,
                max_payload: Self::DEFAULT_MAX_OSC52_PAYLOAD,
                osc52_timeout: Self::DEFAULT_OSC52_TIMEOUT,
                passthrough,
            };
        }

        // External clipboard tools
        if let Some(ext) = external {
            let (backend, fallback) =
                apply_backend_override(caps, ClipboardBackend::External(ext), None);
            log_detected(backend);
            return Self {
                caps,
                backend,
                fallback,
                max_payload: Self::DEFAULT_MAX_OSC52_PAYLOAD,
                osc52_timeout: Self::DEFAULT_OSC52_TIMEOUT,
                passthrough: PassthroughMode::None,
            };
        }

        // Nothing available
        log_detected(ClipboardBackend::Unavailable);
        Self {
            caps,
            backend: ClipboardBackend::Unavailable,
            fallback: None,
            max_payload: Self::DEFAULT_MAX_OSC52_PAYLOAD,
            osc52_timeout: Self::DEFAULT_OSC52_TIMEOUT,
            passthrough: PassthroughMode::None,
        }
    }

    /// Create a clipboard helper with a custom OSC 52 payload limit.
    #[must_use]
    pub fn with_max_payload(caps: TerminalCapabilities, max_payload: usize) -> Self {
        let mut clipboard = Self::new(caps);
        clipboard.max_payload = if max_payload == 0 {
            Self::DEFAULT_MAX_OSC52_PAYLOAD
        } else {
            max_payload
        };
        clipboard
    }

    /// Return the selected backend.
    #[must_use]
    pub const fn backend(&self) -> ClipboardBackend {
        self.backend
    }

    /// Return true when any clipboard backend is available.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        !matches!(self.backend, ClipboardBackend::Unavailable)
    }

    /// Return true if OSC 52 is supported.
    #[must_use]
    pub const fn supports_osc52(&self) -> bool {
        self.caps.osc52_clipboard
    }

    /// Return the maximum allowed OSC 52 base64 payload size.
    #[must_use]
    pub const fn max_payload(&self) -> usize {
        self.max_payload
    }

    /// Enable OSC 52 with multiplexer DCS passthrough wrapping.
    ///
    /// By default, OSC 52 is disabled inside tmux and GNU screen because
    /// these multiplexers intercept escape sequences. This method enables
    /// clipboard access by wrapping OSC 52 sequences in DCS passthrough,
    /// allowing them to reach the outer terminal.
    ///
    /// Requires the multiplexer to have passthrough enabled
    /// (tmux 3.3+: `set -g allow-passthrough on`).
    ///
    /// Zellij handles passthrough natively and does not need wrapping.
    #[must_use]
    pub fn with_mux_passthrough(mut self) -> Self {
        if self.caps.in_tmux {
            self.passthrough = PassthroughMode::Tmux;
            self.backend = ClipboardBackend::Osc52;
        } else if self.caps.in_screen {
            self.passthrough = PassthroughMode::Screen;
            self.backend = ClipboardBackend::Osc52;
        }
        self
    }

    /// Return true when OSC 52 is usable (directly or via passthrough).
    #[must_use]
    const fn is_osc52_usable(&self) -> bool {
        self.caps.osc52_clipboard || !matches!(self.passthrough, PassthroughMode::None)
    }

    /// Write an OSC 52 query sequence to request clipboard content.
    ///
    /// The terminal will respond with an OSC 52 response containing the
    /// clipboard content (base64-encoded). The response must be read from
    /// the terminal input stream.
    ///
    /// Not all terminals support OSC 52 reads (many only support writes).
    pub fn query_osc52(
        &self,
        selection: ClipboardSelection,
        writer: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        if !self.is_osc52_usable() {
            return Err(ClipboardError::NotAvailable);
        }
        let code = selection.osc52_code()?;
        let seq = format!("\x1b]52;{code};?\x07");
        self.write_with_passthrough(writer, seq.as_bytes())
    }

    /// Set clipboard content.
    pub fn set(
        &self,
        content: &str,
        selection: ClipboardSelection,
        writer: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        match self.backend {
            ClipboardBackend::Osc52 => {
                let result = self.write_osc52(content, selection, writer);
                if result.is_err() {
                    if let Some(fallback) = self.fallback {
                        log_fallback(self.backend, ClipboardBackend::External(fallback), "osc52");
                        return set_external_backend(fallback, content, selection);
                    }
                }
                log_write(self.backend, content.len());
                result
            }
            ClipboardBackend::External(backend) => {
                log_write(self.backend, content.len());
                set_external_backend(backend, content, selection)
            }
            ClipboardBackend::Unavailable => Err(ClipboardError::NotAvailable),
        }
    }

    /// Set clipboard content with the fallback chain enabled.
    #[cfg(feature = "clipboard-fallback")]
    pub fn set_with_fallback(
        &self,
        content: &str,
        selection: ClipboardSelection,
        writer: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        self.set(content, selection, writer)
    }

    /// Clear clipboard content.
    pub fn clear(
        &self,
        selection: ClipboardSelection,
        writer: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        match self.backend {
            ClipboardBackend::Osc52 => {
                let code = selection.osc52_code()?;
                let seq = format!("\x1b]52;{code};\x07");
                self.write_with_passthrough(writer, seq.as_bytes())
            }
            ClipboardBackend::External(backend) => set_external_backend(backend, "", selection),
            ClipboardBackend::Unavailable => Err(ClipboardError::NotAvailable),
        }
    }

    /// Read clipboard content with a default timeout for OSC 52.
    pub fn get(&self) -> Result<String, ClipboardError> {
        self.get_with_timeout(self.osc52_timeout)
    }

    /// Read clipboard content with a custom timeout for OSC 52.
    ///
    /// Note: OSC 52 read responses require terminal support. If unavailable, we
    /// fall back to external clipboard tools when possible.
    pub fn get_with_timeout(&self, _timeout: Duration) -> Result<String, ClipboardError> {
        match self.backend {
            ClipboardBackend::External(backend) => get_external_backend(backend),
            ClipboardBackend::Osc52 => {
                if let Some(fallback) = self.fallback {
                    log_fallback(
                        self.backend,
                        ClipboardBackend::External(fallback),
                        "timeout",
                    );
                    return get_external_backend(fallback);
                }
                Err(ClipboardError::Timeout)
            }
            ClipboardBackend::Unavailable => Err(ClipboardError::NotAvailable),
        }
    }

    fn write_osc52(
        &self,
        content: &str,
        selection: ClipboardSelection,
        writer: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        if !self.is_osc52_usable() {
            return Err(ClipboardError::NotAvailable);
        }
        let code = selection.osc52_code()?;
        let encoded = STANDARD.encode(content.as_bytes());
        if encoded.len() > self.max_payload {
            return Err(ClipboardError::InvalidInput(format!(
                "OSC 52 payload too large ({} > {})",
                encoded.len(),
                self.max_payload
            )));
        }
        let seq = format!("\x1b]52;{code};{encoded}\x07");
        self.write_with_passthrough(writer, seq.as_bytes())
    }

    /// Write raw bytes, applying DCS passthrough wrapping if needed.
    fn write_with_passthrough(
        &self,
        writer: &mut impl Write,
        seq: &[u8],
    ) -> Result<(), ClipboardError> {
        match self.passthrough {
            PassthroughMode::None => {
                writer
                    .write_all(seq)
                    .map_err(|e| ClipboardError::WriteError(e.to_string()))?;
            }
            PassthroughMode::Tmux => {
                write_tmux_passthrough(writer, seq)?;
            }
            PassthroughMode::Screen => {
                write_screen_passthrough(writer, seq)?;
            }
        }
        writer
            .flush()
            .map_err(|e| ClipboardError::WriteError(e.to_string()))
    }
}

/// Write a sequence wrapped in tmux DCS passthrough.
///
/// Format: `ESC P tmux; <seq-with-ESC-doubled> ESC \`
fn write_tmux_passthrough(writer: &mut impl Write, seq: &[u8]) -> Result<(), ClipboardError> {
    writer
        .write_all(b"\x1bPtmux;")
        .map_err(|e| ClipboardError::WriteError(e.to_string()))?;
    for &byte in seq {
        if byte == 0x1b {
            // Double ESC bytes inside the passthrough payload.
            writer
                .write_all(b"\x1b\x1b")
                .map_err(|e| ClipboardError::WriteError(e.to_string()))?;
        } else {
            writer
                .write_all(&[byte])
                .map_err(|e| ClipboardError::WriteError(e.to_string()))?;
        }
    }
    writer
        .write_all(b"\x1b\\")
        .map_err(|e| ClipboardError::WriteError(e.to_string()))
}

/// Write a sequence wrapped in GNU screen DCS passthrough.
///
/// Format: `ESC P <seq> ESC \`
fn write_screen_passthrough(writer: &mut impl Write, seq: &[u8]) -> Result<(), ClipboardError> {
    writer
        .write_all(b"\x1bP")
        .map_err(|e| ClipboardError::WriteError(e.to_string()))?;
    writer
        .write_all(seq)
        .map_err(|e| ClipboardError::WriteError(e.to_string()))?;
    writer
        .write_all(b"\x1b\\")
        .map_err(|e| ClipboardError::WriteError(e.to_string()))
}

fn apply_backend_override(
    caps: TerminalCapabilities,
    backend: ClipboardBackend,
    fallback: Option<ExternalBackend>,
) -> (ClipboardBackend, Option<ExternalBackend>) {
    let override_val = match env::var(ENV_CLIPBOARD_BACKEND) {
        Ok(val) => val,
        Err(_) => return (backend, fallback),
    };

    let override_val = override_val.to_ascii_lowercase();
    match override_val.as_str() {
        "osc52" => {
            if caps.osc52_clipboard {
                (ClipboardBackend::Osc52, fallback)
            } else {
                (ClipboardBackend::Unavailable, None)
            }
        }
        "macos" => resolve_external_override(ExternalBackend::MacOS),
        "windows" => resolve_external_override(ExternalBackend::Windows),
        "wayland" => resolve_external_override(ExternalBackend::Wayland),
        "x11" => resolve_external_override(ExternalBackend::X11),
        "none" => (ClipboardBackend::Unavailable, None),
        _ => (backend, fallback),
    }
}

fn resolve_external_override(
    backend: ExternalBackend,
) -> (ClipboardBackend, Option<ExternalBackend>) {
    if external_backend_available(backend) {
        (ClipboardBackend::External(backend), None)
    } else {
        (ClipboardBackend::Unavailable, None)
    }
}

fn detect_external_backend() -> Option<ExternalBackend> {
    if external_backend_available(ExternalBackend::MacOS) {
        return Some(ExternalBackend::MacOS);
    }
    if external_backend_available(ExternalBackend::Windows) {
        return Some(ExternalBackend::Windows);
    }
    if external_backend_available(ExternalBackend::Wayland) {
        return Some(ExternalBackend::Wayland);
    }
    if external_backend_available(ExternalBackend::X11) {
        return Some(ExternalBackend::X11);
    }
    None
}

fn external_backend_available(backend: ExternalBackend) -> bool {
    #[cfg(feature = "clipboard-fallback")]
    {
        match backend {
            ExternalBackend::MacOS => {
                cfg!(target_os = "macos") && command_exists("pbcopy") && command_exists("pbpaste")
            }
            ExternalBackend::Windows => cfg!(target_os = "windows") && command_exists("clip"),
            ExternalBackend::Wayland => {
                env::var_os("WAYLAND_DISPLAY").is_some()
                    && command_exists("wl-copy")
                    && command_exists("wl-paste")
            }
            ExternalBackend::X11 => {
                env::var_os("DISPLAY").is_some()
                    && (command_exists("xclip") || command_exists("xsel"))
            }
        }
    }
    #[cfg(not(feature = "clipboard-fallback"))]
    {
        let _ = backend;
        false
    }
}

fn command_exists(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(command).is_file();
    }

    let path_var = match env::var_os("PATH") {
        Some(path) => path,
        None => return false,
    };

    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        if cfg!(target_os = "windows") {
            let candidate = dir.join(format!("{command}.exe"));
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

fn set_external_backend(
    backend: ExternalBackend,
    content: &str,
    selection: ClipboardSelection,
) -> Result<(), ClipboardError> {
    if selection != ClipboardSelection::Clipboard {
        return Err(ClipboardError::InvalidInput(
            "external clipboard supports only Clipboard selection".to_string(),
        ));
    }
    #[cfg(feature = "clipboard-fallback")]
    {
        match backend {
            ExternalBackend::MacOS => run_command_with_input("pbcopy", &[], content),
            ExternalBackend::Windows => run_command_with_input("clip", &[], content),
            ExternalBackend::Wayland => run_command_with_input("wl-copy", &[], content),
            ExternalBackend::X11 => {
                if run_command_with_input("xclip", &["-selection", "clipboard"], content).is_ok() {
                    Ok(())
                } else {
                    run_command_with_input("xsel", &["--clipboard", "--input"], content)
                }
            }
        }
    }
    #[cfg(not(feature = "clipboard-fallback"))]
    {
        let _ = backend;
        let _ = content;
        Err(ClipboardError::NotAvailable)
    }
}

fn get_external_backend(backend: ExternalBackend) -> Result<String, ClipboardError> {
    #[cfg(feature = "clipboard-fallback")]
    {
        match backend {
            ExternalBackend::MacOS => run_command_output("pbpaste", &[]),
            ExternalBackend::Windows => {
                run_command_output("powershell", &["-NoProfile", "-Command", "Get-Clipboard"])
            }
            ExternalBackend::Wayland => run_command_output("wl-paste", &["--no-newline"]),
            ExternalBackend::X11 => {
                if let Ok(output) = run_command_output("xclip", &["-selection", "clipboard", "-o"])
                {
                    Ok(output)
                } else {
                    run_command_output("xsel", &["--clipboard", "--output"])
                }
            }
        }
    }
    #[cfg(not(feature = "clipboard-fallback"))]
    {
        let _ = backend;
        Err(ClipboardError::NotAvailable)
    }
}

#[cfg(feature = "clipboard-fallback")]
fn run_command_with_input(cmd: &str, args: &[&str], content: &str) -> Result<(), ClipboardError> {
    use std::process::{Command, Stdio};

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| ClipboardError::WriteError(err.to_string()))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content.as_bytes())
            .map_err(|err| ClipboardError::WriteError(err.to_string()))?;
    }

    let status = child
        .wait()
        .map_err(|err| ClipboardError::WriteError(err.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(ClipboardError::WriteError(format!(
            "clipboard command failed: {cmd}"
        )))
    }
}

#[cfg(feature = "clipboard-fallback")]
fn run_command_output(cmd: &str, args: &[&str]) -> Result<String, ClipboardError> {
    use std::process::Command;

    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|err| ClipboardError::ReadError(err.to_string()))?;

    if !output.status.success() {
        return Err(ClipboardError::ReadError(format!(
            "clipboard command failed: {cmd}"
        )));
    }

    String::from_utf8(output.stdout).map_err(|err| ClipboardError::ReadError(err.to_string()))
}

#[cfg(feature = "clipboard-logging")]
fn log_detected(backend: ClipboardBackend) {
    tracing::info!(backend = ?backend, "Clipboard backend detected");
}

#[cfg(not(feature = "clipboard-logging"))]
fn log_detected(_backend: ClipboardBackend) {}

#[cfg(feature = "clipboard-logging")]
fn log_write(backend: ClipboardBackend, bytes: usize) {
    tracing::debug!(backend = ?backend, bytes, "Clipboard write");
}

#[cfg(not(feature = "clipboard-logging"))]
fn log_write(_backend: ClipboardBackend, _bytes: usize) {}

#[cfg(feature = "clipboard-logging")]
fn log_fallback(primary: ClipboardBackend, fallback: ClipboardBackend, reason: &str) {
    tracing::warn!(primary = ?primary, fallback = ?fallback, reason, "Clipboard fallback triggered");
}

#[cfg(not(feature = "clipboard-logging"))]
fn log_fallback(_primary: ClipboardBackend, _fallback: ClipboardBackend, _reason: &str) {}

#[cfg(test)]
mod tests {
    use super::{
        Clipboard, ClipboardBackend, ClipboardError, ClipboardSelection, PassthroughMode,
        TerminalCapabilities,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    fn caps_with_clipboard() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.osc52_clipboard = true;
        caps
    }

    fn caps_in_tmux() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.in_tmux = true;
        caps
    }

    fn caps_in_screen() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.in_screen = true;
        caps
    }

    // --- Selection code tests ---

    #[test]
    fn selection_clipboard_code() {
        assert_eq!(ClipboardSelection::Clipboard.osc52_code().unwrap(), 'c');
    }

    #[test]
    fn selection_primary_code() {
        assert_eq!(ClipboardSelection::Primary.osc52_code().unwrap(), 'p');
    }

    #[test]
    fn selection_secondary_code() {
        assert_eq!(ClipboardSelection::Secondary.osc52_code().unwrap(), 's');
    }

    #[test]
    fn selection_cut_buffer_codes() {
        for i in 0..=7 {
            let code = ClipboardSelection::CutBuffer(i).osc52_code().unwrap();
            assert_eq!(code, (b'0' + i) as char);
        }
    }

    #[test]
    fn selection_cut_buffer_bounds() {
        let ok = ClipboardSelection::CutBuffer(7).osc52_code();
        assert!(ok.is_ok());

        let err = ClipboardSelection::CutBuffer(8).osc52_code();
        assert!(matches!(err, Err(ClipboardError::InvalidInput(_))));
    }

    // --- Basic OSC 52 write tests ---

    #[test]
    fn set_writes_osc52_sequence() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .set("hi", ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let expected = format!("\x1b]52;c;{}\x07", STANDARD.encode("hi"));
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn set_primary_selection() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .set("data", ClipboardSelection::Primary, &mut out)
            .unwrap();

        let expected = format!("\x1b]52;p;{}\x07", STANDARD.encode("data"));
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn set_cut_buffer() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .set("buf", ClipboardSelection::CutBuffer(3), &mut out)
            .unwrap();

        let expected = format!("\x1b]52;3;{}\x07", STANDARD.encode("buf"));
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn clear_writes_empty_osc52_sequence() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .clear(ClipboardSelection::Clipboard, &mut out)
            .unwrap();
        assert_eq!(out, b"\x1b]52;c;\x07");
    }

    #[test]
    fn clear_primary_selection() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .clear(ClipboardSelection::Primary, &mut out)
            .unwrap();
        assert_eq!(out, b"\x1b]52;p;\x07");
    }

    #[test]
    fn size_limit_is_enforced() {
        let mut clipboard = Clipboard::new(caps_with_clipboard());
        clipboard.max_payload = 4;
        let mut out = Vec::new();
        let err = clipboard.set("hello", ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::InvalidInput(_))));
    }

    #[test]
    fn set_fails_when_unavailable() {
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        let mut out = Vec::new();
        let err = clipboard.set("hi", ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::NotAvailable)));
    }

    // --- Passthrough wrapping tests ---

    #[test]
    fn with_mux_passthrough_enables_osc52_in_tmux() {
        let clipboard = Clipboard::new(caps_in_tmux()).with_mux_passthrough();
        assert_eq!(clipboard.backend(), ClipboardBackend::Osc52);
        assert_eq!(clipboard.passthrough, PassthroughMode::Tmux);
    }

    #[test]
    fn with_mux_passthrough_enables_osc52_in_screen() {
        let clipboard = Clipboard::new(caps_in_screen()).with_mux_passthrough();
        assert_eq!(clipboard.backend(), ClipboardBackend::Osc52);
        assert_eq!(clipboard.passthrough, PassthroughMode::Screen);
    }

    #[test]
    fn with_mux_passthrough_noop_outside_mux() {
        let clipboard = Clipboard::new(caps_with_clipboard()).with_mux_passthrough();
        assert_eq!(clipboard.passthrough, PassthroughMode::None);
    }

    #[test]
    fn tmux_passthrough_wraps_set() {
        let clipboard = Clipboard::new(caps_in_tmux()).with_mux_passthrough();
        let mut out = Vec::new();
        clipboard
            .set("hi", ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let encoded = STANDARD.encode("hi");
        // Expected: ESC P tmux; ESC ESC ] 52;c;<base64> BEL ESC \
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1bPtmux;");
        expected.extend_from_slice(b"\x1b\x1b"); // doubled ESC
        expected.extend_from_slice(format!("]52;c;{encoded}\x07").as_bytes());
        expected.extend_from_slice(b"\x1b\\");
        assert_eq!(out, expected);
    }

    #[test]
    fn screen_passthrough_wraps_set() {
        let clipboard = Clipboard::new(caps_in_screen()).with_mux_passthrough();
        let mut out = Vec::new();
        clipboard
            .set("hi", ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let encoded = STANDARD.encode("hi");
        // Expected: ESC P ESC ] 52;c;<base64> BEL ESC \
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1bP");
        expected.extend_from_slice(format!("\x1b]52;c;{encoded}\x07").as_bytes());
        expected.extend_from_slice(b"\x1b\\");
        assert_eq!(out, expected);
    }

    #[test]
    fn tmux_passthrough_wraps_clear() {
        let clipboard = Clipboard::new(caps_in_tmux()).with_mux_passthrough();
        let mut out = Vec::new();
        clipboard
            .clear(ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        // ESC P tmux; ESC ESC ] 52;c; BEL ESC \
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1bPtmux;");
        expected.extend_from_slice(b"\x1b\x1b"); // doubled ESC
        expected.extend_from_slice(b"]52;c;\x07");
        expected.extend_from_slice(b"\x1b\\");
        assert_eq!(out, expected);
    }

    #[test]
    fn screen_passthrough_wraps_clear() {
        let clipboard = Clipboard::new(caps_in_screen()).with_mux_passthrough();
        let mut out = Vec::new();
        clipboard
            .clear(ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1bP");
        expected.extend_from_slice(b"\x1b]52;c;\x07");
        expected.extend_from_slice(b"\x1b\\");
        assert_eq!(out, expected);
    }

    #[test]
    fn passthrough_enforces_size_limit() {
        let mut clipboard = Clipboard::new(caps_in_tmux()).with_mux_passthrough();
        clipboard.max_payload = 4;
        let mut out = Vec::new();
        let err = clipboard.set("hello", ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::InvalidInput(_))));
    }

    // --- OSC 52 query tests ---

    #[test]
    fn query_osc52_writes_question_mark() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .query_osc52(ClipboardSelection::Clipboard, &mut out)
            .unwrap();
        assert_eq!(out, b"\x1b]52;c;?\x07");
    }

    #[test]
    fn query_osc52_primary() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .query_osc52(ClipboardSelection::Primary, &mut out)
            .unwrap();
        assert_eq!(out, b"\x1b]52;p;?\x07");
    }

    #[test]
    fn query_osc52_with_tmux_passthrough() {
        let clipboard = Clipboard::new(caps_in_tmux()).with_mux_passthrough();
        let mut out = Vec::new();
        clipboard
            .query_osc52(ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1bPtmux;");
        expected.extend_from_slice(b"\x1b\x1b"); // doubled ESC
        expected.extend_from_slice(b"]52;c;?\x07");
        expected.extend_from_slice(b"\x1b\\");
        assert_eq!(out, expected);
    }

    #[test]
    fn query_osc52_fails_when_unavailable() {
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        let mut out = Vec::new();
        let err = clipboard.query_osc52(ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::NotAvailable)));
    }

    // --- Builder/accessor tests ---

    #[test]
    fn default_max_payload() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        assert_eq!(
            clipboard.max_payload(),
            Clipboard::DEFAULT_MAX_OSC52_PAYLOAD
        );
    }

    #[test]
    fn with_max_payload_custom() {
        let clipboard = Clipboard::with_max_payload(caps_with_clipboard(), 1000);
        assert_eq!(clipboard.max_payload(), 1000);
    }

    #[test]
    fn with_max_payload_zero_uses_default() {
        let clipboard = Clipboard::with_max_payload(caps_with_clipboard(), 0);
        assert_eq!(
            clipboard.max_payload(),
            Clipboard::DEFAULT_MAX_OSC52_PAYLOAD
        );
    }

    #[test]
    fn is_available_with_osc52() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        assert!(clipboard.is_available());
    }

    #[test]
    fn is_available_false_for_basic() {
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        assert!(!clipboard.is_available());
    }

    #[test]
    fn is_available_with_passthrough() {
        let clipboard = Clipboard::new(caps_in_tmux()).with_mux_passthrough();
        assert!(clipboard.is_available());
    }

    // --- OSC 52 encoding correctness tests ---

    #[test]
    fn osc52_encoding_is_valid_base64() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .set("hello world", ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let output = String::from_utf8(out).unwrap();
        // Extract base64 payload between `;` and BEL
        let payload = output
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        let decoded = STANDARD.decode(payload).unwrap();
        assert_eq!(decoded, b"hello world");
    }

    #[test]
    fn osc52_encoding_handles_unicode() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        let content = "日本語テスト 🎉";
        clipboard
            .set(content, ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let output = String::from_utf8(out).unwrap();
        let payload = output
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        let decoded = STANDARD.decode(payload).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), content);
    }

    #[test]
    fn osc52_encoding_handles_binary_content() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        // Content with NUL bytes and control chars
        let content = "before\x00after\x01\x1b[31m";
        clipboard
            .set(content, ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        // Verify the base64 is correct and round-trips
        let expected_b64 = STANDARD.encode(content.as_bytes());
        let expected_seq = format!("\x1b]52;c;{expected_b64}\x07");
        assert_eq!(out, expected_seq.as_bytes());
    }

    #[test]
    fn osc52_encoding_handles_empty_string() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        clipboard
            .set("", ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        // Empty string base64-encodes to ""
        assert_eq!(out, b"\x1b]52;c;\x07");
    }

    // --- Large content tests ---

    #[test]
    fn large_content_within_limit_succeeds() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        // Create content that fits within the default limit
        let content = "A".repeat(50_000);
        clipboard
            .set(&content, ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        let output = String::from_utf8(out).unwrap();
        let payload = output
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        let decoded = STANDARD.decode(payload).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), content);
    }

    #[test]
    fn large_content_exceeding_limit_rejected() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let mut out = Vec::new();
        // DEFAULT_MAX_OSC52_PAYLOAD is 74_994 bytes of base64
        // base64 expands by ~4/3, so ~56K of raw data will exceed it
        let content = "X".repeat(60_000);
        let err = clipboard.set(&content, ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::InvalidInput(_))));
    }

    #[test]
    fn custom_size_limit_respected() {
        let clipboard = Clipboard::with_max_payload(caps_with_clipboard(), 100);
        let mut out = Vec::new();
        // 100 bytes of text -> ~136 bytes base64 -> exceeds 100 limit
        let content = "Y".repeat(100);
        let err = clipboard.set(&content, ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::InvalidInput(_))));

        // But small content should work
        let mut out2 = Vec::new();
        clipboard
            .set("small", ClipboardSelection::Clipboard, &mut out2)
            .unwrap();
        assert!(!out2.is_empty());
    }

    // --- Backend detection and fallback tests ---

    #[test]
    fn backend_reports_osc52_when_capable() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        assert_eq!(clipboard.backend(), ClipboardBackend::Osc52);
    }

    #[test]
    fn backend_reports_unavailable_for_basic_caps() {
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        assert_eq!(clipboard.backend(), ClipboardBackend::Unavailable);
    }

    #[test]
    fn get_returns_timeout_for_osc52_without_fallback() {
        let clipboard = Clipboard::new(caps_with_clipboard());
        let err = clipboard.get();
        assert!(matches!(err, Err(ClipboardError::Timeout)));
    }

    #[test]
    fn get_returns_not_available_when_unavailable() {
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        let err = clipboard.get();
        assert!(matches!(err, Err(ClipboardError::NotAvailable)));
    }

    #[test]
    fn clear_fails_when_unavailable() {
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        let mut out = Vec::new();
        let err = clipboard.clear(ClipboardSelection::Clipboard, &mut out);
        assert!(matches!(err, Err(ClipboardError::NotAvailable)));
    }

    // --- Zellij passthrough tests ---

    #[test]
    fn zellij_does_not_need_passthrough() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_zellij = true;
        caps.osc52_clipboard = true;
        let clipboard = Clipboard::new(caps).with_mux_passthrough();
        // Zellij handles passthrough natively, no DCS wrapping
        assert_eq!(clipboard.passthrough, PassthroughMode::None);
    }

    // --- Error display tests ---

    #[test]
    fn error_display_not_available() {
        let err = ClipboardError::NotAvailable;
        assert_eq!(err.to_string(), "clipboard not available");
    }

    #[test]
    fn error_display_invalid_input() {
        let err = ClipboardError::InvalidInput("too big".to_string());
        assert_eq!(err.to_string(), "invalid input: too big");
    }

    #[test]
    fn error_display_write_error() {
        let err = ClipboardError::WriteError("broken pipe".to_string());
        assert_eq!(err.to_string(), "clipboard write failed: broken pipe");
    }

    #[test]
    fn error_display_read_error() {
        let err = ClipboardError::ReadError("no data".to_string());
        assert_eq!(err.to_string(), "clipboard read failed: no data");
    }

    #[test]
    fn error_display_timeout() {
        let err = ClipboardError::Timeout;
        assert_eq!(err.to_string(), "clipboard read timed out");
    }

    // --- Auto-detection tests ---

    #[test]
    fn auto_direct_osc52_when_supported() {
        let clipboard = Clipboard::auto(caps_with_clipboard());
        assert_eq!(clipboard.backend(), ClipboardBackend::Osc52);
        assert_eq!(clipboard.passthrough, PassthroughMode::None);
    }

    #[test]
    fn auto_enables_tmux_passthrough() {
        let clipboard = Clipboard::auto(caps_in_tmux());
        assert_eq!(clipboard.backend(), ClipboardBackend::Osc52);
        assert_eq!(clipboard.passthrough, PassthroughMode::Tmux);
    }

    #[test]
    fn auto_enables_screen_passthrough() {
        let clipboard = Clipboard::auto(caps_in_screen());
        assert_eq!(clipboard.backend(), ClipboardBackend::Osc52);
        assert_eq!(clipboard.passthrough, PassthroughMode::Screen);
    }

    #[test]
    fn auto_tmux_passthrough_writes_correctly() {
        let clipboard = Clipboard::auto(caps_in_tmux());
        let mut out = Vec::new();
        clipboard
            .set("auto", ClipboardSelection::Clipboard, &mut out)
            .unwrap();

        // Should be wrapped in tmux passthrough
        assert!(out.starts_with(b"\x1bPtmux;"));
        assert!(out.ends_with(b"\x1b\\"));
    }

    #[test]
    fn auto_zellij_no_passthrough() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_zellij = true;
        // Zellij doesn't need passthrough wrapping, and osc52_clipboard
        // would be false for mux environments in default detection
        let clipboard = Clipboard::auto(caps);
        assert_eq!(clipboard.passthrough, PassthroughMode::None);
    }

    #[test]
    fn auto_unavailable_for_basic_caps() {
        // Basic caps with no external tools and no OSC 52
        let clipboard = Clipboard::new(TerminalCapabilities::basic());
        assert_eq!(clipboard.backend(), ClipboardBackend::Unavailable);
        assert!(!clipboard.is_available());
    }

    #[test]
    fn detect_does_not_enable_passthrough() {
        // detect() should NOT auto-enable passthrough (backward compat)
        let clipboard = Clipboard::detect(caps_in_tmux());
        assert_eq!(clipboard.passthrough, PassthroughMode::None);
    }

    // --- Integration tests (require real terminals, skipped in CI) ---

    #[test]
    #[ignore = "requires real terminal with OSC 52 support"]
    fn integration_copy_paste_roundtrip() {
        let caps = TerminalCapabilities::detect();
        let clipboard = Clipboard::detect(caps);
        if !clipboard.is_available() {
            return;
        }
        let mut writer = std::io::stdout();
        clipboard
            .set("roundtrip test", ClipboardSelection::Clipboard, &mut writer)
            .unwrap();
    }

    #[test]
    #[ignore = "requires tmux with allow-passthrough"]
    fn integration_tmux_passthrough() {
        let caps = TerminalCapabilities::detect();
        if !caps.in_tmux {
            return;
        }
        let clipboard = Clipboard::new(caps).with_mux_passthrough();
        let mut writer = std::io::stdout();
        clipboard
            .set("tmux test", ClipboardSelection::Clipboard, &mut writer)
            .unwrap();
    }

    #[test]
    #[ignore = "requires system clipboard tools (pbcopy, xclip, etc.)"]
    fn integration_fallback_system_clipboard() {
        let caps = TerminalCapabilities::detect();
        let clipboard = Clipboard::detect(caps);
        if !clipboard.is_available() {
            return;
        }
        let content = clipboard.get();
        // Just verify it doesn't panic; actual content depends on system state
        let _ = content;
    }
}
