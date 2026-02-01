#![forbid(unsafe_code)]

//! Feature-gated terminal capability probing.
//!
//! This module provides optional runtime probing of terminal capabilities
//! using device attribute queries and OSC sequences. It refines the
//! environment-based detection from [`TerminalCapabilities::detect`].
//!
//! # Safety Contract
//!
//! - **Bounded timeouts**: Every probe has a hard timeout (default 500ms).
//!   On timeout, the probe returns `None` (fail-open).
//! - **Fail-open**: Unrecognized or malformed responses are treated as
//!   "unknown" — the corresponding capability remains unchanged.
//! - **One-writer rule**: Probing must only run when `TerminalSession` is
//!   active and before the event loop starts. The caller is responsible
//!   for ensuring exclusive terminal ownership.
//!
//! # Platform Support
//!
//! Runtime probing requires direct `/dev/tty` access and is only available
//! on Unix platforms. On non-Unix targets, [`probe_capabilities`] returns
//! an empty [`ProbeResult`].
//!
//! # Usage
//!
//! ```no_run
//! use ftui_core::caps_probe::{probe_capabilities, ProbeConfig};
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//!
//! let mut caps = TerminalCapabilities::detect();
//! let result = probe_capabilities(&ProbeConfig::default());
//! caps.refine_from_probe(&result);
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::terminal_capabilities::TerminalCapabilities;

/// Maximum bytes to read in a single probe response.
const MAX_RESPONSE_LEN: usize = 256;

/// Default per-probe timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// Configuration for terminal probing.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Timeout per individual probe query.
    pub timeout: Duration,
    /// Whether to probe DA1 (Primary Device Attributes).
    pub probe_da1: bool,
    /// Whether to probe DA2 (Secondary Device Attributes).
    pub probe_da2: bool,
    /// Whether to probe background color (dark/light detection).
    ///
    /// Opt-in because some terminals may show visual artifacts
    /// from the OSC 11 query.
    pub probe_background: bool,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            probe_da1: true,
            probe_da2: true,
            probe_background: false,
        }
    }
}

/// Results from terminal probing.
///
/// Each field is `Option<T>`: `Some` means the probe succeeded and
/// returned a definitive answer; `None` means the probe timed out
/// or returned an unrecognizable response (fail-open).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// DA1 attribute codes reported by the terminal.
    ///
    /// Common values:
    /// - 3 = ReGIS graphics
    /// - 4 = Sixel graphics
    /// - 6 = selective erase
    /// - 22 = ANSI color
    pub da1_attributes: Option<Vec<u32>>,

    /// Terminal type identifier from DA2.
    ///
    /// Known values: 0=VT100, 1=VT220, 41=xterm, 65=VT520,
    /// 77=mintty, 83=screen, 84=tmux, 85=rxvt-unicode.
    pub da2_terminal_type: Option<u32>,

    /// Terminal firmware/version from DA2.
    pub da2_version: Option<u32>,

    /// Whether the terminal background appears dark.
    ///
    /// Determined by probing the background color via OSC 11 and
    /// computing perceived luminance.
    pub dark_background: Option<bool>,
}

/// Probe terminal capabilities at runtime.
///
/// Sends device attribute queries to the terminal and parses responses
/// to refine capability detection beyond what environment variables
/// can provide.
///
/// # Requirements
///
/// - Terminal must be in raw mode (`TerminalSession` active).
/// - No event loop should be running (probes read from the tty).
/// - Call this during session initialization, before event processing.
///
/// # Fail-Open Guarantee
///
/// If any probe times out or returns unrecognizable data, the
/// corresponding field in [`ProbeResult`] is `None` and the existing
/// capabilities remain unchanged.
pub fn probe_capabilities(config: &ProbeConfig) -> ProbeResult {
    #[cfg(unix)]
    return probe_capabilities_unix(config);

    #[cfg(not(unix))]
    {
        let _ = config;
        ProbeResult::default()
    }
}

#[cfg(unix)]
fn probe_capabilities_unix(config: &ProbeConfig) -> ProbeResult {
    let mut result = ProbeResult::default();

    if config.probe_da1 {
        result.da1_attributes = probe_da1(config.timeout);
    }

    if config.probe_da2
        && let Some((term_type, version)) = probe_da2(config.timeout)
    {
        result.da2_terminal_type = Some(term_type);
        result.da2_version = Some(version);
    }

    if config.probe_background {
        result.dark_background = probe_background_color(config.timeout);
    }

    result
}

// --- DA1: Primary Device Attributes ---
//
// Query:    ESC [ c
// Response: ESC [ ? Ps ; Ps ; ... c
//
// Attribute codes:
//   1 = 132 columns     4 = Sixel graphics
//   2 = printer port     6 = selective erase
//   3 = ReGIS graphics   22 = ANSI color

#[cfg(unix)]
const DA1_QUERY: &[u8] = b"\x1b[c";

#[cfg(unix)]
fn probe_da1(timeout: Duration) -> Option<Vec<u32>> {
    let response = send_probe(DA1_QUERY, timeout)?;
    parse_da1_response(&response)
}

/// Parse a DA1 response into a list of attribute codes.
fn parse_da1_response(bytes: &[u8]) -> Option<Vec<u32>> {
    // Expected: ESC [ ? Ps ; Ps ; ... c
    let start = find_subsequence(bytes, b"\x1b[?")?;
    let payload = &bytes[start + 3..];

    let end = payload.iter().position(|&b| b == b'c')?;
    let params = &payload[..end];

    let attrs: Vec<u32> = params
        .split(|&b| b == b';')
        .filter_map(|chunk| {
            let s = std::str::from_utf8(chunk).ok()?;
            s.trim().parse().ok()
        })
        .collect();

    if attrs.is_empty() { None } else { Some(attrs) }
}

// --- DA2: Secondary Device Attributes ---
//
// Query:    ESC [ > c
// Response: ESC [ > Pp ; Pv ; Pc c
//
// Pp = terminal type:
//   0 = VT100, 1 = VT220, 2 = VT240, 41 = xterm,
//   65 = VT520, 77 = mintty, 83 = screen, 84 = tmux,
//   85 = rxvt-unicode
//
// Pv = firmware version
// Pc = ROM cartridge registration (usually 0)

#[cfg(unix)]
const DA2_QUERY: &[u8] = b"\x1b[>c";

#[cfg(unix)]
fn probe_da2(timeout: Duration) -> Option<(u32, u32)> {
    let response = send_probe(DA2_QUERY, timeout)?;
    parse_da2_response(&response)
}

/// Parse a DA2 response into (terminal_type, version).
fn parse_da2_response(bytes: &[u8]) -> Option<(u32, u32)> {
    // Expected: ESC [ > Pp ; Pv ; Pc c
    let start = find_subsequence(bytes, b"\x1b[>")?;
    let payload = &bytes[start + 3..];

    let end = payload.iter().position(|&b| b == b'c')?;
    let params = &payload[..end];

    let parts: Vec<u32> = params
        .split(|&b| b == b';')
        .filter_map(|chunk| {
            let s = std::str::from_utf8(chunk).ok()?;
            s.trim().parse().ok()
        })
        .collect();

    match parts.len() {
        0 | 1 => None,
        _ => Some((parts[0], parts[1])),
    }
}

/// Map DA2 terminal type ID to a human-readable name.
#[must_use]
pub fn da2_id_to_name(id: u32) -> &'static str {
    match id {
        0 => "vt100",
        1 => "vt220",
        2 => "vt240",
        41 => "xterm",
        65 => "vt520",
        77 => "mintty",
        83 => "screen",
        84 => "tmux",
        85 => "rxvt-unicode",
        _ => "unknown",
    }
}

// --- Background Color Probe ---
//
// Query:    OSC 11 ; ? ST  (ESC ] 11 ; ? ESC \)
// Response: OSC 11 ; rgb:RRRR/GGGG/BBBB ST
//
// Used for dark/light mode detection via perceived luminance.

#[cfg(unix)]
const BG_COLOR_QUERY: &[u8] = b"\x1b]11;?\x1b\\";

#[cfg(unix)]
fn probe_background_color(timeout: Duration) -> Option<bool> {
    let response = send_probe(BG_COLOR_QUERY, timeout)?;
    parse_background_response(&response)
}

/// Parse an OSC 11 background color response to determine dark/light.
///
/// Returns `Some(true)` for dark backgrounds, `Some(false)` for light,
/// `None` if the response is unparseable.
fn parse_background_response(bytes: &[u8]) -> Option<bool> {
    let s = std::str::from_utf8(bytes).ok()?;

    let rgb_start = s.find("rgb:")?;
    let rgb_data = &s[rgb_start + 4..];

    let parts: Vec<&str> = rgb_data
        .split('/')
        .map(|p| {
            // Trim non-hex trailing characters (ST, BEL, etc.)
            let end = p.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(p.len());
            &p[..end]
        })
        .collect();

    if parts.len() < 3 {
        return None;
    }

    let r = parse_color_component(parts[0])?;
    let g = parse_color_component(parts[1])?;
    let b = parse_color_component(parts[2])?;

    // Determine scale: 4-digit hex (0-65535) or 2-digit hex (0-255).
    let max_val: f64 = if parts[0].len() > 2 { 65535.0 } else { 255.0 };

    // Perceived luminance (ITU-R BT.601).
    let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
    let normalized = luminance / max_val;

    Some(normalized < 0.5)
}

/// Parse a hex color component (2- or 4-digit).
fn parse_color_component(s: &str) -> Option<u16> {
    if s.is_empty() {
        return None;
    }
    u16::from_str_radix(s, 16).ok()
}

// --- Probe I/O (Unix only) ---
//
// We open /dev/tty directly for both reading and writing to avoid
// interfering with crossterm's internal event reader, which also
// uses /dev/tty but through its own file descriptor.

#[cfg(unix)]
fn send_probe(query: &[u8], timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Write;

    // Write query directly to the tty.
    let mut tty_write = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty_write.write_all(query).ok()?;
    tty_write.flush().ok()?;
    drop(tty_write);

    read_tty_response(timeout)
}

/// Read a response from /dev/tty with a hard timeout.
///
/// Uses a background thread to perform the blocking read. If the
/// response is not received within `timeout`, returns `None`.
///
/// The background thread reads byte-by-byte and checks for response
/// completeness markers (CSI terminator or OSC string terminator).
#[cfg(unix)]
fn read_tty_response(timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    let tty = std::fs::File::open("/dev/tty").ok()?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(1);

    // Clone timeout for the thread's internal guard.
    let thread_timeout = timeout + Duration::from_millis(200);

    thread::Builder::new()
        .name("ftui-caps-probe".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(tty);
            let mut response = Vec::with_capacity(64);
            let mut buf = [0u8; 1];
            let start = Instant::now();

            while response.len() < MAX_RESPONSE_LEN {
                match reader.read(&mut buf) {
                    Ok(1) => {
                        response.push(buf[0]);
                        if is_response_complete(&response) {
                            break;
                        }
                    }
                    _ => break,
                }
                // Belt-and-suspenders: internal timeout guard.
                if start.elapsed() > thread_timeout {
                    break;
                }
            }

            let _ = tx.send(response);
        })
        .ok()?;

    match rx.recv_timeout(timeout) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// Check if a byte sequence represents a complete terminal response.
///
/// Recognizes:
/// - CSI responses: `ESC [ ... <alpha>` (e.g., DA1/DA2 ending in `c`)
/// - OSC responses: `ESC ] ... BEL` or `ESC ] ... ESC \`
fn is_response_complete(buf: &[u8]) -> bool {
    if buf.len() < 3 {
        return false;
    }

    // CSI response: ESC [ ... <alphabetic>
    if buf[0] == 0x1b && buf[1] == b'[' {
        let last = buf[buf.len() - 1];
        return last.is_ascii_alphabetic();
    }

    // OSC response: ESC ] ... BEL  or  ESC ] ... ESC \
    if buf[0] == 0x1b && buf[1] == b']' {
        let last = buf[buf.len() - 1];
        if last == 0x07 {
            return true; // BEL terminator
        }
        if buf.len() >= 4 {
            let second_last = buf[buf.len() - 2];
            if second_last == 0x1b && last == b'\\' {
                return true; // ST terminator
            }
        }
    }

    false
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// =========================================================================
// Capability Auto-Upgrade (bd-3227)
// =========================================================================

/// Capabilities that can be probed and confirmed at runtime.
///
/// Each variant maps to a terminal feature that environment-variable
/// detection may underestimate. Runtime probing can upgrade (never
/// downgrade) these capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeableCapability {
    /// True color (24-bit RGB) support.
    TrueColor,
    /// Synchronized output (DEC private mode 2026).
    SynchronizedOutput,
    /// OSC 8 hyperlinks.
    Hyperlinks,
    /// Kitty keyboard protocol.
    KittyKeyboard,
    /// Sixel graphics support (DA1 attribute 4).
    Sixel,
    /// Focus event reporting.
    FocusEvents,
}

impl ProbeableCapability {
    /// All probeable capabilities.
    pub const ALL: &'static [Self] = &[
        Self::TrueColor,
        Self::SynchronizedOutput,
        Self::Hyperlinks,
        Self::KittyKeyboard,
        Self::Sixel,
        Self::FocusEvents,
    ];
}

/// Result of a single capability probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Terminal confirmed support for this capability.
    Confirmed,
    /// Terminal explicitly denied support.
    Denied,
    /// No response within timeout — assume unsupported (fail-open).
    Timeout,
    /// Probe has been sent but no response yet.
    Pending,
}

/// Unique identifier for a pending probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProbeId(u32);

/// Tracks capability probes and manages the upgrade lifecycle.
///
/// # Usage Pattern
///
/// 1. Create a `CapabilityProber` during session init (after raw mode).
/// 2. Call [`send_all_probes`] to emit queries for missing capabilities.
/// 3. Feed incoming terminal bytes to [`process_response`].
/// 4. Call [`check_timeouts`] periodically.
/// 5. Apply confirmed upgrades to `TerminalCapabilities`.
///
/// # Upgrade-Only Guarantee
///
/// The prober only ever upgrades capabilities. If environment detection
/// already enabled a feature, it stays enabled regardless of probe results.
#[derive(Debug)]
pub struct CapabilityProber {
    /// Capabilities confirmed by probing.
    confirmed: Vec<ProbeableCapability>,
    /// Capabilities explicitly denied by the terminal.
    denied: Vec<ProbeableCapability>,
    /// Pending probes awaiting responses.
    pending: HashMap<ProbeId, (Instant, ProbeableCapability)>,
    /// Timeout for each probe.
    timeout: Duration,
    /// Counter for generating unique probe IDs.
    next_id: u32,
}

impl CapabilityProber {
    /// Create a new prober with the given per-probe timeout.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self {
            confirmed: Vec::new(),
            denied: Vec::new(),
            pending: HashMap::new(),
            timeout,
            next_id: 0,
        }
    }

    /// Check whether a capability has been confirmed.
    #[must_use]
    pub fn is_confirmed(&self, cap: ProbeableCapability) -> bool {
        self.confirmed.contains(&cap)
    }

    /// Check whether a capability has been denied.
    #[must_use]
    pub fn is_denied(&self, cap: ProbeableCapability) -> bool {
        self.denied.contains(&cap)
    }

    /// Number of probes still awaiting responses.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// All confirmed capabilities.
    pub fn confirmed_capabilities(&self) -> &[ProbeableCapability] {
        &self.confirmed
    }

    /// Send all probes for capabilities not already present in `caps`.
    ///
    /// Returns the number of probes sent.
    ///
    /// # Errors
    ///
    /// Returns `Err` if writing to the terminal fails. Partial writes
    /// are tolerated — successfully sent probes are tracked.
    pub fn send_all_probes(
        &mut self,
        caps: &TerminalCapabilities,
        writer: &mut dyn std::io::Write,
    ) -> std::io::Result<usize> {
        let mut count = 0;

        for &cap in ProbeableCapability::ALL {
            if self.capability_already_detected(cap, caps) {
                continue;
            }
            if let Some(query) = probe_query_for(cap) {
                let id = self.next_probe_id();
                writer.write_all(query)?;
                self.pending.insert(id, (Instant::now(), cap));
                count += 1;
            }
        }
        writer.flush()?;
        Ok(count)
    }

    /// Process a terminal response buffer looking for probe answers.
    ///
    /// This should be called with bytes received from the terminal.
    /// Multiple responses in a single buffer are supported.
    pub fn process_response(&mut self, response: &[u8]) {
        // Check for DECRPM (mode status report): ESC [ ? <mode> ; <status> $ y
        if let Some((mode, status)) = parse_decrpm_response(response) {
            self.handle_mode_report(mode, status);
        }

        // Check DA1 for Sixel (attribute 4)
        if let Some(attrs) = parse_da1_response(response)
            && attrs.contains(&4)
        {
            self.confirm(ProbeableCapability::Sixel);
        }

        // Check DA2 for terminal identification → infer capabilities
        if let Some((term_type, _version)) = parse_da2_response(response) {
            self.infer_from_terminal_type(term_type);
        }
    }

    /// Expire probes that have exceeded their timeout.
    ///
    /// Timed-out probes are treated as "unsupported" (fail-open).
    pub fn check_timeouts(&mut self) {
        let now = Instant::now();
        let timed_out: Vec<ProbeId> = self
            .pending
            .iter()
            .filter(|(_, (sent, _))| now.duration_since(*sent) > self.timeout)
            .map(|(&id, _)| id)
            .collect();

        for id in timed_out {
            self.pending.remove(&id);
            // Timeout = assume unsupported (conservative fail-open).
        }
    }

    /// Apply confirmed upgrades to the given capabilities.
    ///
    /// This only enables features — it never disables them. Call this
    /// after [`process_response`] and [`check_timeouts`] to update the
    /// capability set with probe-confirmed features.
    pub fn apply_upgrades(&self, caps: &mut TerminalCapabilities) {
        for &cap in &self.confirmed {
            match cap {
                ProbeableCapability::TrueColor => {
                    caps.true_color = true;
                    caps.colors_256 = true;
                }
                ProbeableCapability::SynchronizedOutput => {
                    caps.sync_output = true;
                }
                ProbeableCapability::Hyperlinks => {
                    caps.osc8_hyperlinks = true;
                }
                ProbeableCapability::KittyKeyboard => {
                    caps.kitty_keyboard = true;
                }
                ProbeableCapability::Sixel => {
                    // Sixel is informational only — no field in TerminalCapabilities yet.
                }
                ProbeableCapability::FocusEvents => {
                    caps.focus_events = true;
                }
            }
        }
    }

    // --- Internal helpers ---

    fn next_probe_id(&mut self) -> ProbeId {
        let id = ProbeId(self.next_id);
        self.next_id += 1;
        id
    }

    fn confirm(&mut self, cap: ProbeableCapability) {
        if !self.confirmed.contains(&cap) {
            self.confirmed.push(cap);
        }
        // Remove from pending if it was there.
        self.pending.retain(|_, (_, c)| *c != cap);
    }

    fn deny(&mut self, cap: ProbeableCapability) {
        if !self.denied.contains(&cap) {
            self.denied.push(cap);
        }
        self.pending.retain(|_, (_, c)| *c != cap);
    }

    fn capability_already_detected(
        &self,
        cap: ProbeableCapability,
        caps: &TerminalCapabilities,
    ) -> bool {
        match cap {
            ProbeableCapability::TrueColor => caps.true_color,
            ProbeableCapability::SynchronizedOutput => caps.sync_output,
            ProbeableCapability::Hyperlinks => caps.osc8_hyperlinks,
            ProbeableCapability::KittyKeyboard => caps.kitty_keyboard,
            ProbeableCapability::Sixel => false, // No field; always probe.
            ProbeableCapability::FocusEvents => caps.focus_events,
        }
    }

    fn handle_mode_report(&mut self, mode: u32, status: u32) {
        // DECRPM status: 1=set, 2=reset, 3=permanently set, 4=permanently reset, 0=unknown
        match mode {
            2026 => {
                // Synchronized output
                if status == 1 || status == 2 || status == 3 || status == 4 {
                    // Terminal recognizes the mode (even if currently reset).
                    self.confirm(ProbeableCapability::SynchronizedOutput);
                } else {
                    // Status 0 = mode not recognized.
                    self.deny(ProbeableCapability::SynchronizedOutput);
                }
            }
            2004 => {
                // Bracketed paste — informational, not tracked as ProbeableCapability.
            }
            1004 => {
                // Focus events
                if status == 1 || status == 2 || status == 3 || status == 4 {
                    self.confirm(ProbeableCapability::FocusEvents);
                }
            }
            _ => {}
        }
    }

    /// Infer capabilities from known DA2 terminal type IDs.
    fn infer_from_terminal_type(&mut self, term_type: u32) {
        match term_type {
            // xterm and derivatives
            41 => {
                self.confirm(ProbeableCapability::TrueColor);
                self.confirm(ProbeableCapability::Hyperlinks);
                self.confirm(ProbeableCapability::FocusEvents);
            }
            // VTE-based (GNOME Terminal, Tilix, etc.)
            65 => {
                self.confirm(ProbeableCapability::TrueColor);
                self.confirm(ProbeableCapability::Hyperlinks);
            }
            // mintty
            77 => {
                self.confirm(ProbeableCapability::TrueColor);
                self.confirm(ProbeableCapability::Hyperlinks);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// DECRPM (DEC Private Mode Report) support
// ---------------------------------------------------------------------------

/// Query sequence for DECRPM mode status.
///
/// Format: `ESC [ ? <mode> $ p`
#[must_use]
pub fn decrpm_query(mode: u32) -> Vec<u8> {
    format!("\x1b[?{mode}$p").into_bytes()
}

/// Parse a DECRPM response.
///
/// Expected format: `ESC [ ? <mode> ; <status> $ y`
///
/// Status values:
/// - 0: not recognized
/// - 1: set
/// - 2: reset
/// - 3: permanently set
/// - 4: permanently reset
///
/// Returns `(mode, status)` or `None` on parse failure.
#[must_use]
pub fn parse_decrpm_response(response: &[u8]) -> Option<(u32, u32)> {
    // Find CSI ? ... $ y pattern
    let start = find_subsequence(response, b"\x1b[?")?;
    let payload = &response[start + 3..];

    // Find terminator: $ y
    let dollar_pos = payload.iter().position(|&b| b == b'$')?;
    if dollar_pos + 1 >= payload.len() || payload[dollar_pos + 1] != b'y' {
        return None;
    }

    let params = &payload[..dollar_pos];
    let parts: Vec<&[u8]> = params.split(|&b| b == b';').collect();
    if parts.len() < 2 {
        return None;
    }

    let mode: u32 = std::str::from_utf8(parts[0]).ok()?.trim().parse().ok()?;
    let status: u32 = std::str::from_utf8(parts[1]).ok()?.trim().parse().ok()?;

    Some((mode, status))
}

/// Return the probe query bytes for a given capability.
///
/// Returns `None` if the capability doesn't have a direct query
/// (it may be inferred from DA1/DA2 responses instead).
fn probe_query_for(cap: ProbeableCapability) -> Option<&'static [u8]> {
    match cap {
        ProbeableCapability::TrueColor => Some(DA2_QUERY),
        ProbeableCapability::SynchronizedOutput => {
            // DECRPM for mode 2026 — needs dynamic construction.
            // For now, fall back to DA2 inference.
            None
        }
        ProbeableCapability::Hyperlinks => Some(DA2_QUERY),
        ProbeableCapability::KittyKeyboard => None, // Inferred from DA2 terminal type.
        ProbeableCapability::Sixel => Some(DA1_QUERY),
        ProbeableCapability::FocusEvents => None, // Inferred from DA2.
    }
}

// Use the unix-only query constants when available.
#[cfg(not(unix))]
const DA1_QUERY: &[u8] = b"\x1b[c";
#[cfg(not(unix))]
const DA2_QUERY: &[u8] = b"\x1b[>c";

// --- Integration with TerminalCapabilities ---

impl TerminalCapabilities {
    /// Refine capabilities using runtime probe results.
    ///
    /// Only fields where the probe returned a definitive answer are
    /// updated. Fields where the probe timed out or returned
    /// unrecognizable data remain unchanged (fail-open).
    pub fn refine_from_probe(&mut self, result: &ProbeResult) {
        // DA2 terminal identification can detect multiplexers that
        // weren't caught by environment variables.
        if let Some(term_type) = result.da2_terminal_type {
            match term_type {
                83 => self.in_screen = true, // GNU screen
                84 => self.in_tmux = true,   // tmux
                _ => {}
            }
        }

        // DA1 attributes can confirm feature support.
        if let Some(ref attrs) = result.da1_attributes {
            // Attribute 22 indicates ANSI color support.
            if attrs.contains(&22) && !self.colors_256 {
                self.colors_256 = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- DA1 parsing tests ---

    #[test]
    fn parse_da1_basic() {
        // VT220 response: ESC [ ? 1 ; 2 ; 4 c
        let response = b"\x1b[?1;2;4c";
        let attrs = parse_da1_response(response).unwrap();
        assert_eq!(attrs, vec![1, 2, 4]);
    }

    #[test]
    fn parse_da1_single_attr() {
        let response = b"\x1b[?6c";
        let attrs = parse_da1_response(response).unwrap();
        assert_eq!(attrs, vec![6]);
    }

    #[test]
    fn parse_da1_sixel_and_regis() {
        let response = b"\x1b[?1;2;3;4;6c";
        let attrs = parse_da1_response(response).unwrap();
        assert!(attrs.contains(&3)); // ReGIS
        assert!(attrs.contains(&4)); // Sixel
    }

    #[test]
    fn parse_da1_with_leading_garbage() {
        // Response might have stray bytes before the actual response.
        let mut data = Vec::new();
        data.extend_from_slice(b"garbage");
        data.extend_from_slice(b"\x1b[?1;4c");
        let attrs = parse_da1_response(&data).unwrap();
        assert_eq!(attrs, vec![1, 4]);
    }

    #[test]
    fn parse_da1_empty_response() {
        assert!(parse_da1_response(b"").is_none());
    }

    #[test]
    fn parse_da1_malformed_no_question_mark() {
        let response = b"\x1b[1;2c";
        assert!(parse_da1_response(response).is_none());
    }

    #[test]
    fn parse_da1_malformed_no_terminator() {
        let response = b"\x1b[?1;2;4";
        assert!(parse_da1_response(response).is_none());
    }

    #[test]
    fn parse_da1_malformed_garbage() {
        let response = b"not a terminal response at all";
        assert!(parse_da1_response(response).is_none());
    }

    // --- DA2 parsing tests ---

    #[test]
    fn parse_da2_xterm() {
        // xterm: ESC [ > 41 ; 354 ; 0 c
        let response = b"\x1b[>41;354;0c";
        let (term_type, version) = parse_da2_response(response).unwrap();
        assert_eq!(term_type, 41);
        assert_eq!(version, 354);
    }

    #[test]
    fn parse_da2_vt100() {
        let response = b"\x1b[>0;115;0c";
        let (term_type, version) = parse_da2_response(response).unwrap();
        assert_eq!(term_type, 0);
        assert_eq!(version, 115);
    }

    #[test]
    fn parse_da2_mintty() {
        let response = b"\x1b[>77;30600;0c";
        let (term_type, version) = parse_da2_response(response).unwrap();
        assert_eq!(term_type, 77);
        assert_eq!(version, 30600);
    }

    #[test]
    fn parse_da2_two_params() {
        // Some terminals omit the third parameter.
        let response = b"\x1b[>1;220c";
        let (term_type, version) = parse_da2_response(response).unwrap();
        assert_eq!(term_type, 1);
        assert_eq!(version, 220);
    }

    #[test]
    fn parse_da2_with_leading_garbage() {
        let mut data = Vec::new();
        data.extend_from_slice(b"junk");
        data.extend_from_slice(b"\x1b[>41;354;0c");
        let (term_type, version) = parse_da2_response(&data).unwrap();
        assert_eq!(term_type, 41);
        assert_eq!(version, 354);
    }

    #[test]
    fn parse_da2_empty_response() {
        assert!(parse_da2_response(b"").is_none());
    }

    #[test]
    fn parse_da2_malformed_single_param() {
        let response = b"\x1b[>41c";
        assert!(parse_da2_response(response).is_none());
    }

    #[test]
    fn parse_da2_malformed_no_terminator() {
        let response = b"\x1b[>41;354;0";
        assert!(parse_da2_response(response).is_none());
    }

    // --- DA2 ID to name ---

    #[test]
    fn da2_known_names() {
        assert_eq!(da2_id_to_name(0), "vt100");
        assert_eq!(da2_id_to_name(41), "xterm");
        assert_eq!(da2_id_to_name(77), "mintty");
        assert_eq!(da2_id_to_name(83), "screen");
        assert_eq!(da2_id_to_name(84), "tmux");
        assert_eq!(da2_id_to_name(85), "rxvt-unicode");
    }

    #[test]
    fn da2_unknown_id() {
        assert_eq!(da2_id_to_name(999), "unknown");
    }

    // --- Background color parsing tests ---

    #[test]
    fn parse_bg_dark() {
        // Dark background: rgb:0000/0000/0000 (black)
        let response = b"\x1b]11;rgb:0000/0000/0000\x1b\\";
        assert_eq!(parse_background_response(response), Some(true));
    }

    #[test]
    fn parse_bg_light() {
        // Light background: rgb:ffff/ffff/ffff (white)
        let response = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\";
        assert_eq!(parse_background_response(response), Some(false));
    }

    #[test]
    fn parse_bg_dark_solarized() {
        // Solarized Dark base03: #002b36 → rgb:0000/2b2b/3636
        let response = b"\x1b]11;rgb:0000/2b2b/3636\x1b\\";
        assert_eq!(parse_background_response(response), Some(true));
    }

    #[test]
    fn parse_bg_light_solarized() {
        // Solarized Light base3: #fdf6e3 → rgb:fdfd/f6f6/e3e3
        let response = b"\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\";
        assert_eq!(parse_background_response(response), Some(false));
    }

    #[test]
    fn parse_bg_bel_terminator() {
        // Some terminals use BEL instead of ST.
        let response = b"\x1b]11;rgb:0000/0000/0000\x07";
        assert_eq!(parse_background_response(response), Some(true));
    }

    #[test]
    fn parse_bg_two_digit_hex() {
        // Some terminals report 2-digit hex: rgb:00/00/00
        let response = b"\x1b]11;rgb:00/00/00\x1b\\";
        assert_eq!(parse_background_response(response), Some(true));

        let response = b"\x1b]11;rgb:ff/ff/ff\x1b\\";
        assert_eq!(parse_background_response(response), Some(false));
    }

    #[test]
    fn parse_bg_empty_response() {
        assert!(parse_background_response(b"").is_none());
    }

    #[test]
    fn parse_bg_malformed_no_rgb() {
        let response = b"\x1b]11;something\x1b\\";
        assert!(parse_background_response(response).is_none());
    }

    #[test]
    fn parse_bg_malformed_incomplete_rgb() {
        let response = b"\x1b]11;rgb:0000/0000\x1b\\";
        assert!(parse_background_response(response).is_none());
    }

    // --- Color component parsing ---

    #[test]
    fn parse_component_four_digit() {
        assert_eq!(parse_color_component("ffff"), Some(0xffff));
        assert_eq!(parse_color_component("0000"), Some(0));
        assert_eq!(parse_color_component("8080"), Some(0x8080));
    }

    #[test]
    fn parse_component_two_digit() {
        assert_eq!(parse_color_component("ff"), Some(0xff));
        assert_eq!(parse_color_component("00"), Some(0));
        assert_eq!(parse_color_component("80"), Some(0x80));
    }

    #[test]
    fn parse_component_empty() {
        assert!(parse_color_component("").is_none());
    }

    #[test]
    fn parse_component_invalid() {
        assert!(parse_color_component("zzzz").is_none());
    }

    // --- Response completeness ---

    #[test]
    fn response_complete_csi() {
        assert!(is_response_complete(b"\x1b[?1;2c"));
        assert!(is_response_complete(b"\x1b[>41;354c"));
    }

    #[test]
    fn response_complete_osc_bel() {
        assert!(is_response_complete(b"\x1b]11;rgb:0/0/0\x07"));
    }

    #[test]
    fn response_complete_osc_st() {
        assert!(is_response_complete(b"\x1b]11;rgb:0/0/0\x1b\\"));
    }

    #[test]
    fn response_incomplete_csi() {
        assert!(!is_response_complete(b"\x1b[?1;2"));
        assert!(!is_response_complete(b"\x1b["));
    }

    #[test]
    fn response_incomplete_osc() {
        assert!(!is_response_complete(b"\x1b]11;rgb:0/0/0"));
    }

    #[test]
    fn response_incomplete_too_short() {
        assert!(!is_response_complete(b""));
        assert!(!is_response_complete(b"\x1b"));
        assert!(!is_response_complete(b"\x1b["));
    }

    // --- Subsequence finder ---

    #[test]
    fn find_subseq_present() {
        assert_eq!(find_subsequence(b"hello world", b"world"), Some(6));
        assert_eq!(find_subsequence(b"\x1b[?1c", b"\x1b[?"), Some(0));
    }

    #[test]
    fn find_subseq_absent() {
        assert!(find_subsequence(b"hello", b"world").is_none());
        assert!(find_subsequence(b"", b"x").is_none());
    }

    #[test]
    fn find_subseq_at_start() {
        assert_eq!(find_subsequence(b"abc", b"ab"), Some(0));
    }

    // --- ProbeConfig defaults ---

    #[test]
    fn default_config() {
        let config = ProbeConfig::default();
        assert_eq!(config.timeout, Duration::from_millis(500));
        assert!(config.probe_da1);
        assert!(config.probe_da2);
        assert!(!config.probe_background);
    }

    // --- ProbeResult defaults ---

    #[test]
    fn default_result_is_all_none() {
        let result = ProbeResult::default();
        assert!(result.da1_attributes.is_none());
        assert!(result.da2_terminal_type.is_none());
        assert!(result.da2_version.is_none());
        assert!(result.dark_background.is_none());
    }

    // --- Refine integration ---

    #[test]
    fn refine_empty_result_is_noop() {
        let mut caps = TerminalCapabilities::basic();
        let original = caps;
        caps.refine_from_probe(&ProbeResult::default());
        assert_eq!(caps, original);
    }

    #[test]
    fn refine_detects_tmux_from_da2() {
        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.in_tmux);

        let result = ProbeResult {
            da2_terminal_type: Some(84), // tmux
            ..ProbeResult::default()
        };
        caps.refine_from_probe(&result);
        assert!(caps.in_tmux);
    }

    #[test]
    fn refine_detects_screen_from_da2() {
        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.in_screen);

        let result = ProbeResult {
            da2_terminal_type: Some(83), // screen
            ..ProbeResult::default()
        };
        caps.refine_from_probe(&result);
        assert!(caps.in_screen);
    }

    #[test]
    fn refine_upgrades_color_from_da1() {
        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.colors_256);

        let result = ProbeResult {
            da1_attributes: Some(vec![1, 6, 22]),
            ..ProbeResult::default()
        };
        caps.refine_from_probe(&result);
        assert!(caps.colors_256);
    }

    #[test]
    fn refine_does_not_downgrade_color() {
        let mut caps = TerminalCapabilities::basic();
        caps.colors_256 = true;

        // DA1 without attribute 22 should NOT downgrade.
        let result = ProbeResult {
            da1_attributes: Some(vec![1, 6]),
            ..ProbeResult::default()
        };
        caps.refine_from_probe(&result);
        assert!(caps.colors_256); // Still true.
    }

    // --- Non-Unix fallback ---

    #[test]
    fn probe_returns_result() {
        // On any platform, probe_capabilities should not panic.
        let result = probe_capabilities(&ProbeConfig::default());
        // On non-Unix (or when /dev/tty is unavailable), result is empty.
        // We just verify it doesn't panic.
        let _ = result;
    }

    // --- ProbeableCapability tests ---

    #[test]
    fn all_capabilities_listed() {
        assert_eq!(ProbeableCapability::ALL.len(), 6);
    }

    // --- DECRPM parser tests ---

    #[test]
    fn parse_decrpm_mode_set() {
        // Mode 2026 (sync output) is set
        let response = b"\x1b[?2026;1$y";
        let (mode, status) = parse_decrpm_response(response).unwrap();
        assert_eq!(mode, 2026);
        assert_eq!(status, 1);
    }

    #[test]
    fn parse_decrpm_mode_reset() {
        // Mode 2026 is reset (but recognized)
        let response = b"\x1b[?2026;2$y";
        let (mode, status) = parse_decrpm_response(response).unwrap();
        assert_eq!(mode, 2026);
        assert_eq!(status, 2);
    }

    #[test]
    fn parse_decrpm_mode_unknown() {
        // Mode not recognized (status 0)
        let response = b"\x1b[?9999;0$y";
        let (mode, status) = parse_decrpm_response(response).unwrap();
        assert_eq!(mode, 9999);
        assert_eq!(status, 0);
    }

    #[test]
    fn parse_decrpm_permanently_set() {
        let response = b"\x1b[?1004;3$y";
        let (mode, status) = parse_decrpm_response(response).unwrap();
        assert_eq!(mode, 1004);
        assert_eq!(status, 3);
    }

    #[test]
    fn parse_decrpm_with_noise() {
        let mut data = Vec::new();
        data.extend_from_slice(b"noise");
        data.extend_from_slice(b"\x1b[?2026;1$y");
        let (mode, status) = parse_decrpm_response(&data).unwrap();
        assert_eq!(mode, 2026);
        assert_eq!(status, 1);
    }

    #[test]
    fn parse_decrpm_empty() {
        assert!(parse_decrpm_response(b"").is_none());
    }

    #[test]
    fn parse_decrpm_malformed_no_dollar_y() {
        assert!(parse_decrpm_response(b"\x1b[?2026;1").is_none());
    }

    #[test]
    fn parse_decrpm_malformed_missing_semicolon() {
        assert!(parse_decrpm_response(b"\x1b[?2026$y").is_none());
    }

    // --- decrpm_query tests ---

    #[test]
    fn decrpm_query_format() {
        let query = decrpm_query(2026);
        assert_eq!(query, b"\x1b[?2026$p");
    }

    // --- CapabilityProber tests ---

    #[test]
    fn prober_new() {
        let prober = CapabilityProber::new(Duration::from_millis(200));
        assert_eq!(prober.pending_count(), 0);
        assert!(prober.confirmed_capabilities().is_empty());
    }

    #[test]
    fn prober_confirm_capability() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.confirm(ProbeableCapability::TrueColor);
        assert!(prober.is_confirmed(ProbeableCapability::TrueColor));
        assert!(!prober.is_confirmed(ProbeableCapability::Sixel));
    }

    #[test]
    fn prober_deny_capability() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.deny(ProbeableCapability::SynchronizedOutput);
        assert!(prober.is_denied(ProbeableCapability::SynchronizedOutput));
        assert!(!prober.is_confirmed(ProbeableCapability::SynchronizedOutput));
    }

    #[test]
    fn prober_process_da2_xterm() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[>41;354;0c");

        assert!(prober.is_confirmed(ProbeableCapability::TrueColor));
        assert!(prober.is_confirmed(ProbeableCapability::Hyperlinks));
        assert!(prober.is_confirmed(ProbeableCapability::FocusEvents));
    }

    #[test]
    fn prober_process_da2_vte() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[>65;6500;1c");

        assert!(prober.is_confirmed(ProbeableCapability::TrueColor));
        assert!(prober.is_confirmed(ProbeableCapability::Hyperlinks));
    }

    #[test]
    fn prober_process_da1_sixel() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[?1;2;4c");

        assert!(prober.is_confirmed(ProbeableCapability::Sixel));
    }

    #[test]
    fn prober_process_decrpm_sync_output() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[?2026;1$y");

        assert!(prober.is_confirmed(ProbeableCapability::SynchronizedOutput));
    }

    #[test]
    fn prober_process_decrpm_sync_denied() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[?2026;0$y");

        assert!(prober.is_denied(ProbeableCapability::SynchronizedOutput));
        assert!(!prober.is_confirmed(ProbeableCapability::SynchronizedOutput));
    }

    #[test]
    fn prober_process_decrpm_focus_events() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[?1004;1$y");

        assert!(prober.is_confirmed(ProbeableCapability::FocusEvents));
    }

    #[test]
    fn prober_process_empty_response() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"");

        assert!(prober.confirmed_capabilities().is_empty());
    }

    #[test]
    fn prober_process_garbage_response() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"random garbage bytes");

        assert!(prober.confirmed_capabilities().is_empty());
    }

    #[test]
    fn prober_apply_upgrades() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.confirm(ProbeableCapability::TrueColor);
        prober.confirm(ProbeableCapability::SynchronizedOutput);
        prober.confirm(ProbeableCapability::Hyperlinks);

        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.true_color);
        assert!(!caps.sync_output);
        assert!(!caps.osc8_hyperlinks);

        prober.apply_upgrades(&mut caps);

        assert!(caps.true_color);
        assert!(caps.colors_256); // Also upgraded with truecolor.
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
    }

    #[test]
    fn prober_apply_upgrades_does_not_downgrade() {
        let prober = CapabilityProber::new(Duration::from_millis(200));
        // Don't confirm anything.

        let mut caps = TerminalCapabilities::basic();
        caps.true_color = true;
        caps.sync_output = true;

        prober.apply_upgrades(&mut caps);

        // Still enabled — upgrades only.
        assert!(caps.true_color);
        assert!(caps.sync_output);
    }

    #[test]
    fn prober_send_skips_detected() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));

        let mut caps = TerminalCapabilities::basic();
        caps.true_color = true;
        caps.osc8_hyperlinks = true;
        caps.focus_events = true;

        let mut buf = Vec::new();
        let count = prober.send_all_probes(&caps, &mut buf).unwrap();

        // TrueColor, Hyperlinks, FocusEvents already detected — should skip them.
        // SynchronizedOutput has no direct query (returns None).
        // KittyKeyboard has no direct query (returns None).
        // Sixel: DA1 query should be sent.
        assert_eq!(count, 1); // Only Sixel (DA1)
    }

    #[test]
    fn prober_send_all_for_basic_caps() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();

        let mut buf = Vec::new();
        let count = prober.send_all_probes(&caps, &mut buf).unwrap();

        // TrueColor → DA2, Hyperlinks → DA2 (duplicate, still counted),
        // Sixel → DA1. SyncOutput/KittyKeyboard/FocusEvents → None.
        assert!(count >= 1);
        assert!(!buf.is_empty());
    }

    #[test]
    fn prober_duplicate_confirm_idempotent() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.confirm(ProbeableCapability::TrueColor);
        prober.confirm(ProbeableCapability::TrueColor);

        assert_eq!(prober.confirmed_capabilities().len(), 1);
    }
}
