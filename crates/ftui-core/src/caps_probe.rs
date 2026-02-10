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
use web_time::{Duration, Instant};

use crate::terminal_capabilities::TerminalCapabilities;

/// Maximum bytes to read in a single probe response.
#[cfg(unix)]
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
#[cfg(unix)]
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

    // Normalize each component based on its hex digit count.
    // X11 color spec supports 1-4 hex digits per component (4/8/12/16-bit).
    fn scale_for_digits(n: usize) -> f64 {
        match n {
            1 => 15.0,
            2 => 255.0,
            3 => 4095.0,
            _ => 65535.0,
        }
    }

    let r_norm = f64::from(r) / scale_for_digits(parts[0].len());
    let g_norm = f64::from(g) / scale_for_digits(parts[1].len());
    let b_norm = f64::from(b) / scale_for_digits(parts[2].len());

    // Perceived luminance (ITU-R BT.601).
    let luminance = 0.299 * r_norm + 0.587 * g_norm + 0.114 * b_norm;

    Some(luminance < 0.5)
}

/// Parse a hex color component (2- or 4-digit).
#[cfg(unix)]
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
#[cfg(unix)]
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

// =========================================================================
// Evidence Ledger: Bayesian capability confidence (bd-4kq0.7.2)
// =========================================================================

/// A single piece of evidence for or against a capability.
#[derive(Debug, Clone)]
pub struct EvidenceEntry {
    /// What kind of evidence this is.
    pub source: EvidenceSource,
    /// Log-odds contribution (positive = supports, negative = refutes).
    pub log_odds: f64,
}

/// Source of a capability evidence observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    /// Environment variable detection (e.g., COLORTERM=truecolor).
    Environment,
    /// DA1 primary device attributes response.
    Da1Response,
    /// DA2 secondary device attributes response.
    Da2Response,
    /// DECRPM mode report.
    DecrpmResponse,
    /// OSC 11 background color probe.
    OscResponse,
    /// Probe timed out — weak negative evidence.
    Timeout,
    /// Conservative prior (no evidence).
    Prior,
}

/// Bayesian evidence ledger for a single capability.
///
/// Maintains a log-odds representation of confidence:
/// - `log_odds = 0.0` → 50% confidence (no evidence)
/// - `log_odds > 0` → more likely supported
/// - `log_odds < 0` → more likely unsupported
///
/// The ledger accumulates evidence entries, each contributing an
/// additive log-odds factor. The total log-odds is the sum.
///
/// # Confidence Interpretation
///
/// | Log-odds | Probability | Interpretation |
/// |----------|-------------|----------------|
/// | -4.6     | ~1%         | Very unlikely  |
/// | -2.2     | ~10%        | Unlikely       |
/// | 0.0      | 50%         | No evidence    |
/// | +2.2     | ~90%        | Likely         |
/// | +4.6     | ~99%        | Very likely    |
///
/// # Fail-Open Default
///
/// Missing data yields `log_odds = 0.0` (agnostic). The consumer
/// decides the threshold for enabling a feature (typically > 0).
#[derive(Debug, Clone)]
pub struct CapabilityLedger {
    /// The capability this ledger tracks.
    pub capability: ProbeableCapability,
    /// Accumulated log-odds (sum of all evidence).
    total_log_odds: f64,
    /// Individual evidence entries for inspection.
    entries: Vec<EvidenceEntry>,
}

impl CapabilityLedger {
    /// Create a new ledger with an agnostic prior (log-odds = 0).
    #[must_use]
    pub fn new(capability: ProbeableCapability) -> Self {
        Self {
            capability,
            total_log_odds: 0.0,
            entries: Vec::new(),
        }
    }

    /// Create a ledger with a custom prior log-odds.
    ///
    /// Use negative values for conservative priors (assume unsupported).
    #[must_use]
    pub fn with_prior(capability: ProbeableCapability, prior_log_odds: f64) -> Self {
        let mut ledger = Self::new(capability);
        if prior_log_odds.abs() > f64::EPSILON {
            ledger.record(EvidenceSource::Prior, prior_log_odds);
        }
        ledger
    }

    /// Record a new piece of evidence.
    pub fn record(&mut self, source: EvidenceSource, log_odds: f64) {
        self.total_log_odds += log_odds;
        self.entries.push(EvidenceEntry { source, log_odds });
    }

    /// Total accumulated log-odds.
    #[must_use]
    pub fn log_odds(&self) -> f64 {
        self.total_log_odds
    }

    /// Convert log-odds to probability (0.0–1.0).
    ///
    /// Uses the logistic function: `P = 1 / (1 + e^(-log_odds))`.
    #[must_use]
    pub fn probability(&self) -> f64 {
        logistic(self.total_log_odds)
    }

    /// Whether the evidence supports the capability (log-odds > 0).
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.total_log_odds > 0.0
    }

    /// Whether confidence exceeds a threshold probability.
    #[must_use]
    pub fn confident_at(&self, threshold: f64) -> bool {
        self.probability() >= threshold
    }

    /// Number of evidence entries.
    #[must_use]
    pub fn evidence_count(&self) -> usize {
        self.entries.len()
    }

    /// Inspect all evidence entries.
    pub fn entries(&self) -> &[EvidenceEntry] {
        &self.entries
    }

    /// Reset to agnostic state.
    pub fn clear(&mut self) {
        self.total_log_odds = 0.0;
        self.entries.clear();
    }
}

/// Logistic function: maps log-odds to probability.
fn logistic(log_odds: f64) -> f64 {
    // Clamp to avoid overflow in exp.
    let clamped = log_odds.clamp(-20.0, 20.0);
    1.0 / (1.0 + (-clamped).exp())
}

// --- Standard evidence weights (Bayes factors as log-odds) ---

/// Evidence weights for common probe outcomes.
///
/// These are log Bayes factors: `ln(P(data|H) / P(data|¬H))`.
pub mod evidence_weights {
    /// Environment variable explicitly indicates support (e.g., COLORTERM=truecolor).
    /// Strong positive: ~95% likelihood ratio.
    pub const ENV_POSITIVE: f64 = 3.0;

    /// Environment variable absent but not definitive.
    /// Weak negative: ~40% likelihood ratio.
    pub const ENV_ABSENT: f64 = -0.4;

    /// DA2 terminal type matches known-good terminal.
    /// Moderate positive: ~85% likelihood ratio.
    pub const DA2_KNOWN_TERMINAL: f64 = 1.8;

    /// DA1 attribute code confirms feature.
    /// Strong positive: ~97% likelihood ratio.
    pub const DA1_CONFIRMED: f64 = 3.5;

    /// DECRPM confirms mode is recognized (status 1–4).
    /// Very strong: terminal explicitly reports capability.
    pub const DECRPM_CONFIRMED: f64 = 4.6;

    /// DECRPM denies mode (status 0).
    /// Very strong negative.
    pub const DECRPM_DENIED: f64 = -4.6;

    /// Probe timed out — weak negative evidence (could be slow terminal).
    pub const TIMEOUT: f64 = -0.7;

    /// Multiplexer detected — slight negative for passthrough features.
    pub const MUX_PENALTY: f64 = -0.5;
}

/// Convenience: build a complete evidence ledger from a `CapabilityProber` state.
///
/// This is the primary integration point between the probe lifecycle
/// and the Bayesian confidence model.
impl CapabilityProber {
    /// Build evidence ledgers for all probeable capabilities using
    /// current confirmed/denied/timeout state plus environment data.
    pub fn build_ledgers(&self, caps: &TerminalCapabilities) -> Vec<CapabilityLedger> {
        ProbeableCapability::ALL
            .iter()
            .map(|&cap| self.build_ledger_for(cap, caps))
            .collect()
    }

    /// Build an evidence ledger for a single capability.
    fn build_ledger_for(
        &self,
        cap: ProbeableCapability,
        caps: &TerminalCapabilities,
    ) -> CapabilityLedger {
        let mut ledger = CapabilityLedger::new(cap);

        // 1. Environment evidence.
        if self.capability_already_detected(cap, caps) {
            ledger.record(EvidenceSource::Environment, evidence_weights::ENV_POSITIVE);
        } else {
            ledger.record(EvidenceSource::Environment, evidence_weights::ENV_ABSENT);
        }

        // 2. Probe evidence.
        if self.is_confirmed(cap) {
            // Determine the strongest source.
            let weight = match cap {
                ProbeableCapability::Sixel => evidence_weights::DA1_CONFIRMED,
                ProbeableCapability::SynchronizedOutput | ProbeableCapability::FocusEvents => {
                    evidence_weights::DECRPM_CONFIRMED
                }
                _ => evidence_weights::DA2_KNOWN_TERMINAL,
            };
            let source = match cap {
                ProbeableCapability::Sixel => EvidenceSource::Da1Response,
                ProbeableCapability::SynchronizedOutput | ProbeableCapability::FocusEvents => {
                    EvidenceSource::DecrpmResponse
                }
                _ => EvidenceSource::Da2Response,
            };
            ledger.record(source, weight);
        } else if self.is_denied(cap) {
            ledger.record(
                EvidenceSource::DecrpmResponse,
                evidence_weights::DECRPM_DENIED,
            );
        }
        // Note: probes that timed out are no longer in pending after check_timeouts().
        // We don't add timeout evidence here because we can't distinguish
        // "timed out" from "never probed". The caller should use
        // `record_timeout` explicitly if needed.

        // 3. Multiplexer penalty.
        if caps.in_any_mux() {
            ledger.record(EvidenceSource::Environment, evidence_weights::MUX_PENALTY);
        }

        ledger
    }

    /// Record timeout evidence for a capability that didn't respond.
    pub fn record_timeout_evidence(&self, ledger: &mut CapabilityLedger) {
        ledger.record(EvidenceSource::Timeout, evidence_weights::TIMEOUT);
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

    #[cfg(unix)]
    #[test]
    fn parse_bg_dark() {
        // Dark background: rgb:0000/0000/0000 (black)
        let response = b"\x1b]11;rgb:0000/0000/0000\x1b\\";
        assert_eq!(parse_background_response(response), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_light() {
        // Light background: rgb:ffff/ffff/ffff (white)
        let response = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\";
        assert_eq!(parse_background_response(response), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_dark_solarized() {
        // Solarized Dark base03: #002b36 → rgb:0000/2b2b/3636
        let response = b"\x1b]11;rgb:0000/2b2b/3636\x1b\\";
        assert_eq!(parse_background_response(response), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_light_solarized() {
        // Solarized Light base3: #fdf6e3 → rgb:fdfd/f6f6/e3e3
        let response = b"\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\";
        assert_eq!(parse_background_response(response), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_bel_terminator() {
        // Some terminals use BEL instead of ST.
        let response = b"\x1b]11;rgb:0000/0000/0000\x07";
        assert_eq!(parse_background_response(response), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_two_digit_hex() {
        // Some terminals report 2-digit hex: rgb:00/00/00
        let response = b"\x1b]11;rgb:00/00/00\x1b\\";
        assert_eq!(parse_background_response(response), Some(true));

        let response = b"\x1b]11;rgb:ff/ff/ff\x1b\\";
        assert_eq!(parse_background_response(response), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_empty_response() {
        assert!(parse_background_response(b"").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_malformed_no_rgb() {
        let response = b"\x1b]11;something\x1b\\";
        assert!(parse_background_response(response).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parse_bg_malformed_incomplete_rgb() {
        let response = b"\x1b]11;rgb:0000/0000\x1b\\";
        assert!(parse_background_response(response).is_none());
    }

    // --- Color component parsing ---

    #[cfg(unix)]
    #[test]
    fn parse_component_four_digit() {
        assert_eq!(parse_color_component("ffff"), Some(0xffff));
        assert_eq!(parse_color_component("0000"), Some(0));
        assert_eq!(parse_color_component("8080"), Some(0x8080));
    }

    #[cfg(unix)]
    #[test]
    fn parse_component_two_digit() {
        assert_eq!(parse_color_component("ff"), Some(0xff));
        assert_eq!(parse_color_component("00"), Some(0));
        assert_eq!(parse_color_component("80"), Some(0x80));
    }

    #[cfg(unix)]
    #[test]
    fn parse_component_empty() {
        assert!(parse_color_component("").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parse_component_invalid() {
        assert!(parse_color_component("zzzz").is_none());
    }

    // --- Response completeness ---

    #[cfg(unix)]
    #[test]
    fn response_complete_csi() {
        assert!(is_response_complete(b"\x1b[?1;2c"));
        assert!(is_response_complete(b"\x1b[>41;354c"));
    }

    #[cfg(unix)]
    #[test]
    fn response_complete_osc_bel() {
        assert!(is_response_complete(b"\x1b]11;rgb:0/0/0\x07"));
    }

    #[cfg(unix)]
    #[test]
    fn response_complete_osc_st() {
        assert!(is_response_complete(b"\x1b]11;rgb:0/0/0\x1b\\"));
    }

    #[cfg(unix)]
    #[test]
    fn response_incomplete_csi() {
        assert!(!is_response_complete(b"\x1b[?1;2"));
        assert!(!is_response_complete(b"\x1b["));
    }

    #[cfg(unix)]
    #[test]
    fn response_incomplete_osc() {
        assert!(!is_response_complete(b"\x1b]11;rgb:0/0/0"));
    }

    #[cfg(unix)]
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

    #[test]
    fn probe_config_all_disabled_is_noop() {
        let config = ProbeConfig {
            timeout: Duration::from_millis(1),
            probe_da1: false,
            probe_da2: false,
            probe_background: false,
        };
        let result = probe_capabilities(&config);
        assert_eq!(result, ProbeResult::default());
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

    #[test]
    fn prober_timeouts_clear_pending() {
        let mut prober = CapabilityProber::new(Duration::from_millis(1));
        let caps = TerminalCapabilities::basic();
        let mut buf = Vec::new();
        let sent = prober.send_all_probes(&caps, &mut buf).unwrap();
        assert!(sent > 0);
        assert!(prober.pending_count() > 0);

        std::thread::sleep(Duration::from_millis(2));
        prober.check_timeouts();
        assert_eq!(prober.pending_count(), 0);
    }

    // --- Evidence Ledger tests (bd-4kq0.7.2) ---

    #[test]
    fn unit_prior_update_positive() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::TrueColor);
        assert!(
            (ledger.probability() - 0.5).abs() < 0.01,
            "Agnostic prior should be 50%"
        );

        ledger.record(
            EvidenceSource::Da2Response,
            evidence_weights::DA2_KNOWN_TERMINAL,
        );
        assert!(
            ledger.probability() > 0.5,
            "Positive evidence should increase probability: got {}",
            ledger.probability()
        );
    }

    #[test]
    fn unit_prior_update_negative() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::SynchronizedOutput);
        ledger.record(
            EvidenceSource::DecrpmResponse,
            evidence_weights::DECRPM_DENIED,
        );
        assert!(
            ledger.probability() < 0.5,
            "Negative evidence should decrease probability: got {}",
            ledger.probability()
        );
    }

    #[test]
    fn unit_confidence_monotone_with_evidence() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::TrueColor);
        let mut prev_prob = ledger.probability();

        // Adding positive evidence should monotonically increase probability.
        for _ in 0..5 {
            ledger.record(EvidenceSource::Da2Response, 1.0);
            let new_prob = ledger.probability();
            assert!(
                new_prob >= prev_prob,
                "Probability should be monotone: {} < {}",
                new_prob,
                prev_prob
            );
            prev_prob = new_prob;
        }

        // Final probability should be high.
        assert!(ledger.probability() > 0.95);
    }

    #[test]
    fn unit_confidence_bounds_saturate() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::TrueColor);

        // Extreme positive evidence.
        ledger.record(EvidenceSource::Da1Response, 100.0);
        assert!(
            (ledger.probability() - 1.0).abs() < 0.001,
            "Extreme positive should saturate near 1.0"
        );

        // Extreme negative evidence.
        let mut ledger2 = CapabilityLedger::new(ProbeableCapability::TrueColor);
        ledger2.record(EvidenceSource::Timeout, -100.0);
        assert!(
            ledger2.probability() < 0.001,
            "Extreme negative should saturate near 0.0"
        );
    }

    #[test]
    fn unit_logistic_identity() {
        // logistic(0) = 0.5
        assert!((logistic(0.0) - 0.5).abs() < f64::EPSILON);

        // logistic is symmetric: logistic(x) + logistic(-x) = 1.0
        for &x in &[0.5, 1.0, 2.0, 5.0, 10.0] {
            let sum = logistic(x) + logistic(-x);
            assert!(
                (sum - 1.0).abs() < 1e-10,
                "logistic({}) + logistic({}) = {} (expected 1.0)",
                x,
                -x,
                sum
            );
        }
    }

    #[test]
    fn unit_ledger_with_prior() {
        // Conservative prior: slightly negative.
        let ledger = CapabilityLedger::with_prior(ProbeableCapability::Sixel, -1.0);
        assert!(ledger.probability() < 0.5);
        assert_eq!(ledger.evidence_count(), 1);
        assert_eq!(ledger.entries()[0].source, EvidenceSource::Prior);
    }

    #[test]
    fn unit_ledger_clear_resets() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::TrueColor);
        ledger.record(EvidenceSource::Da2Response, 3.0);
        assert!(ledger.probability() > 0.9);

        ledger.clear();
        assert!((ledger.probability() - 0.5).abs() < 0.01);
        assert_eq!(ledger.evidence_count(), 0);
    }

    #[test]
    fn unit_ledger_entries_inspectable() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::TrueColor);
        ledger.record(EvidenceSource::Environment, evidence_weights::ENV_POSITIVE);
        ledger.record(
            EvidenceSource::Da2Response,
            evidence_weights::DA2_KNOWN_TERMINAL,
        );
        ledger.record(EvidenceSource::Timeout, evidence_weights::TIMEOUT);

        assert_eq!(ledger.evidence_count(), 3);

        let entries = ledger.entries();
        assert_eq!(entries[0].source, EvidenceSource::Environment);
        assert!(entries[0].log_odds > 0.0);
        assert_eq!(entries[1].source, EvidenceSource::Da2Response);
        assert!(entries[1].log_odds > 0.0);
        assert_eq!(entries[2].source, EvidenceSource::Timeout);
        assert!(entries[2].log_odds < 0.0);

        // Log-odds should sum correctly.
        let expected_sum: f64 = entries.iter().map(|e| e.log_odds).sum();
        assert!((ledger.log_odds() - expected_sum).abs() < 1e-10);
    }

    #[test]
    fn unit_ledger_deterministic() {
        // Same evidence in same order → same result.
        let build = || {
            let mut ledger = CapabilityLedger::new(ProbeableCapability::Hyperlinks);
            ledger.record(EvidenceSource::Environment, evidence_weights::ENV_POSITIVE);
            ledger.record(
                EvidenceSource::Da2Response,
                evidence_weights::DA2_KNOWN_TERMINAL,
            );
            ledger.record(EvidenceSource::Timeout, evidence_weights::TIMEOUT);
            ledger.probability()
        };

        let p1 = build();
        let p2 = build();
        assert!(
            (p1 - p2).abs() < f64::EPSILON,
            "Ledger must be deterministic"
        );
    }

    #[test]
    fn unit_evidence_weights_signs() {
        // Positive evidence weights are positive (compile-time checks).
        const { assert!(evidence_weights::ENV_POSITIVE > 0.0) };
        const { assert!(evidence_weights::DA2_KNOWN_TERMINAL > 0.0) };
        const { assert!(evidence_weights::DA1_CONFIRMED > 0.0) };
        const { assert!(evidence_weights::DECRPM_CONFIRMED > 0.0) };

        // Negative evidence weights are negative (compile-time checks).
        const { assert!(evidence_weights::ENV_ABSENT < 0.0) };
        const { assert!(evidence_weights::DECRPM_DENIED < 0.0) };
        const { assert!(evidence_weights::TIMEOUT < 0.0) };
        const { assert!(evidence_weights::MUX_PENALTY < 0.0) };
    }

    #[test]
    fn unit_confident_at_threshold() {
        let mut ledger = CapabilityLedger::new(ProbeableCapability::TrueColor);
        assert!(!ledger.confident_at(0.9));

        ledger.record(
            EvidenceSource::Da2Response,
            evidence_weights::DA2_KNOWN_TERMINAL,
        );
        ledger.record(EvidenceSource::Environment, evidence_weights::ENV_POSITIVE);
        assert!(ledger.confident_at(0.9));
    }

    #[test]
    fn unit_build_ledgers_basic_caps() {
        let prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();
        let ledgers = prober.build_ledgers(&caps);

        assert_eq!(ledgers.len(), ProbeableCapability::ALL.len());

        // With basic caps (nothing detected, nothing confirmed), all should
        // have slightly negative log-odds (ENV_ABSENT only).
        for ledger in &ledgers {
            assert!(
                ledger.log_odds() < 0.0,
                "{:?} should have negative log-odds with basic caps, got {}",
                ledger.capability,
                ledger.log_odds()
            );
        }
    }

    #[test]
    fn unit_build_ledgers_with_env_detection() {
        let prober = CapabilityProber::new(Duration::from_millis(200));
        let mut caps = TerminalCapabilities::basic();
        caps.true_color = true;

        let ledgers = prober.build_ledgers(&caps);
        let tc_ledger = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::TrueColor)
            .unwrap();

        // Should have positive log-odds from environment detection.
        assert!(
            tc_ledger.log_odds() > 0.0,
            "TrueColor with env detection should be positive, got {}",
            tc_ledger.log_odds()
        );
        assert!(tc_ledger.is_supported());
    }

    #[test]
    fn unit_build_ledgers_with_probe_confirmation() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[>41;354;0c"); // xterm DA2
        let caps = TerminalCapabilities::basic();

        let ledgers = prober.build_ledgers(&caps);
        let tc_ledger = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::TrueColor)
            .unwrap();

        // Probe confirmation should add strong positive evidence.
        assert!(
            tc_ledger.probability() > 0.8,
            "Confirmed TrueColor should have high confidence, got {}",
            tc_ledger.probability()
        );
    }

    #[test]
    fn unit_build_ledgers_with_denial() {
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        prober.process_response(b"\x1b[?2026;0$y"); // sync output denied
        let caps = TerminalCapabilities::basic();

        let ledgers = prober.build_ledgers(&caps);
        let sync_ledger = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::SynchronizedOutput)
            .unwrap();

        assert!(
            sync_ledger.probability() < 0.05,
            "Denied SyncOutput should have very low confidence, got {}",
            sync_ledger.probability()
        );
    }

    #[test]
    fn unit_build_ledgers_mux_penalty() {
        let prober = CapabilityProber::new(Duration::from_millis(200));
        let mut caps = TerminalCapabilities::basic();
        caps.in_tmux = true;

        let ledgers = prober.build_ledgers(&caps);

        // All ledgers should have the mux penalty applied.
        for ledger in &ledgers {
            let has_mux_entry = ledger
                .entries()
                .iter()
                .any(|e| e.source == EvidenceSource::Environment && e.log_odds < -0.1);
            assert!(
                has_mux_entry,
                "{:?} should have mux penalty entry",
                ledger.capability
            );
        }
    }

    #[test]
    fn unit_record_timeout_evidence() {
        let prober = CapabilityProber::new(Duration::from_millis(200));
        let mut ledger = CapabilityLedger::new(ProbeableCapability::Sixel);
        let before = ledger.probability();

        prober.record_timeout_evidence(&mut ledger);
        let after = ledger.probability();

        assert!(
            after < before,
            "Timeout evidence should decrease probability: {} -> {}",
            before,
            after
        );
    }
}

// =========================================================================
// Recorded IO Harness + E2E Tests (bd-4kq0.7.3)
// =========================================================================

#[cfg(test)]
mod recorded_harness_tests {
    use super::*;

    /// Recorded probe response captured from a PTY session.
    #[derive(Clone, Copy)]
    struct RecordedResponse {
        label: &'static str,
        bytes: &'static [u8],
    }

    /// Recorded probe fixture captured from a PTY session.
    struct RecordedProbeFixture {
        name: &'static str,
        responses: &'static [RecordedResponse],
    }

    impl RecordedProbeFixture {
        fn feed(&self, prober: &mut CapabilityProber) {
            for response in self.responses {
                prober.process_response(response.bytes);
            }
        }

        fn capture_jsonl(&self) -> Vec<String> {
            self.responses
                .iter()
                .enumerate()
                .map(|(idx, response)| {
                    let hex = bytes_to_hex(response.bytes);
                    let escaped = bytes_to_escaped(response.bytes);
                    format!(
                        r#"{{"fixture":"{}","idx":{},"label":"{}","bytes_hex":"{}","bytes_escaped":"{}","len":{}}}"#,
                        self.name,
                        idx,
                        response.label,
                        hex,
                        escaped,
                        response.bytes.len()
                    )
                })
                .collect()
        }

        fn capture_context(&self) -> String {
            self.capture_jsonl().join("\n")
        }
    }

    fn bytes_to_hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn bytes_to_escaped(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|&b| match b {
                b'\x1b' => "\\x1b".to_string(),
                b'\r' => "\\r".to_string(),
                b'\n' => "\\n".to_string(),
                b'\t' => "\\t".to_string(),
                0x20..=0x7e => (b as char).to_string(),
                _ => format!("\\x{:02x}", b),
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn log_capture(fixture: &RecordedProbeFixture) {
        for line in fixture.capture_jsonl() {
            eprintln!("{}", line);
        }
    }

    // Recorded PTY responses (xterm/minimal/mintty). These are raw bytes captured from
    // a PTY session and stored as deterministic fixtures.
    static XTERM_RESPONSES: &[RecordedResponse] = &[
        RecordedResponse {
            label: "DA1",
            bytes: b"\x1b[?1;2;4;6;22c",
        },
        RecordedResponse {
            label: "DA2",
            bytes: b"\x1b[>41;354;0c",
        },
        RecordedResponse {
            label: "DECRPM 2026",
            bytes: b"\x1b[?2026;1$y",
        },
        RecordedResponse {
            label: "DECRPM 1004",
            bytes: b"\x1b[?1004;1$y",
        },
    ];

    static MINIMAL_VT100_RESPONSES: &[RecordedResponse] = &[
        RecordedResponse {
            label: "DA1",
            bytes: b"\x1b[?1c",
        },
        RecordedResponse {
            label: "DA2",
            bytes: b"\x1b[>0;115;0c",
        },
        RecordedResponse {
            label: "DECRPM 2026",
            bytes: b"\x1b[?2026;0$y",
        },
        RecordedResponse {
            label: "DECRPM 1004",
            bytes: b"\x1b[?1004;0$y",
        },
    ];

    static MINTTY_RESPONSES: &[RecordedResponse] = &[
        RecordedResponse {
            label: "DA1",
            bytes: b"\x1b[?1;2;6;22c",
        },
        RecordedResponse {
            label: "DA2",
            bytes: b"\x1b[>77;30600;0c",
        },
        RecordedResponse {
            label: "DECRPM 2026",
            bytes: b"\x1b[?2026;2$y",
        },
        RecordedResponse {
            label: "DECRPM 1004",
            bytes: b"\x1b[?1004;1$y",
        },
    ];

    static XTERM_FIXTURE: RecordedProbeFixture = RecordedProbeFixture {
        name: "xterm",
        responses: XTERM_RESPONSES,
    };

    static MINIMAL_VT100_FIXTURE: RecordedProbeFixture = RecordedProbeFixture {
        name: "vt100",
        responses: MINIMAL_VT100_RESPONSES,
    };

    static MINTTY_FIXTURE: RecordedProbeFixture = RecordedProbeFixture {
        name: "mintty",
        responses: MINTTY_RESPONSES,
    };

    static TIMEOUT_FIXTURE: RecordedProbeFixture = RecordedProbeFixture {
        name: "timeout",
        responses: &[],
    };

    /// JSONL log record for E2E tracing.
    #[derive(Debug)]
    struct ProbeLogEntry {
        capability: &'static str,
        probe: &'static str,
        response: String,
        decision: &'static str,
        confidence: f64,
        timeout: bool,
    }

    impl ProbeLogEntry {
        fn to_jsonl(&self) -> String {
            format!(
                r#"{{"capability":"{}","probe":"{}","response":"{}","decision":"{}","confidence":{:.4},"timeout":{}}}"#,
                self.capability,
                self.probe,
                self.response,
                self.decision,
                self.confidence,
                self.timeout,
            )
        }
    }

    fn cap_name(cap: ProbeableCapability) -> &'static str {
        match cap {
            ProbeableCapability::TrueColor => "TrueColor",
            ProbeableCapability::SynchronizedOutput => "SynchronizedOutput",
            ProbeableCapability::Hyperlinks => "Hyperlinks",
            ProbeableCapability::KittyKeyboard => "KittyKeyboard",
            ProbeableCapability::Sixel => "Sixel",
            ProbeableCapability::FocusEvents => "FocusEvents",
        }
    }

    fn build_log(ledgers: &[CapabilityLedger], terminal_name: &'static str) -> Vec<ProbeLogEntry> {
        ledgers
            .iter()
            .map(|l| {
                let decision = if l.confident_at(0.9) {
                    "enable"
                } else if l.probability() < 0.1 {
                    "disable"
                } else {
                    "unknown"
                };
                ProbeLogEntry {
                    capability: cap_name(l.capability),
                    probe: terminal_name,
                    response: format!("log_odds={:.2}", l.log_odds()),
                    decision,
                    confidence: l.probability(),
                    timeout: l
                        .entries()
                        .iter()
                        .any(|e| e.source == EvidenceSource::Timeout),
                }
            })
            .collect()
    }

    // --- E2E tests ---

    #[test]
    fn e2e_recorded_probe_success_xterm() {
        let fixture = &XTERM_FIXTURE;
        log_capture(fixture);
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();

        fixture.feed(&mut prober);
        let ledgers = prober.build_ledgers(&caps);
        let capture_context = fixture.capture_context();

        // xterm should enable TrueColor, Hyperlinks, FocusEvents, Sixel, SyncOutput.
        let tc = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::TrueColor)
            .unwrap();
        assert!(
            tc.confident_at(0.8),
            "TrueColor confidence: {:.2}\nCaptured:\n{}",
            tc.probability(),
            capture_context
        );

        let hl = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::Hyperlinks)
            .unwrap();
        assert!(
            hl.confident_at(0.8),
            "Hyperlinks confidence: {:.2}\nCaptured:\n{}",
            hl.probability(),
            capture_context
        );

        let fe = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::FocusEvents)
            .unwrap();
        assert!(
            fe.confident_at(0.8),
            "FocusEvents confidence: {:.2}\nCaptured:\n{}",
            fe.probability(),
            capture_context
        );

        let sx = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::Sixel)
            .unwrap();
        assert!(
            sx.confident_at(0.8),
            "Sixel confidence: {:.2}\nCaptured:\n{}",
            sx.probability(),
            capture_context
        );

        let so = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::SynchronizedOutput)
            .unwrap();
        assert!(
            so.confident_at(0.8),
            "SyncOutput confidence: {:.2}\nCaptured:\n{}",
            so.probability(),
            capture_context
        );

        // JSONL output validation.
        let log = build_log(&ledgers, fixture.name);
        for entry in &log {
            let line = entry.to_jsonl();
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "Bad JSONL: {}",
                line
            );
        }
    }

    #[test]
    fn e2e_recorded_timeout_all() {
        let fixture = &TIMEOUT_FIXTURE;
        log_capture(fixture);
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();

        fixture.feed(&mut prober);
        let capture_context = fixture.capture_context();

        // Add timeout evidence for all capabilities.
        let mut ledgers = prober.build_ledgers(&caps);
        for ledger in &mut ledgers {
            prober.record_timeout_evidence(ledger);
        }

        // With timeout + no env detection, all should be uncertain or disabled.
        for ledger in &ledgers {
            assert!(
                !ledger.confident_at(0.9),
                "{:?} should not be confidently enabled after timeout, confidence: {:.2}\nCaptured:\n{}",
                ledger.capability,
                ledger.probability(),
                capture_context
            );
            // Evidence should include timeout entry.
            assert!(
                ledger
                    .entries()
                    .iter()
                    .any(|e| e.source == EvidenceSource::Timeout),
                "{:?} missing timeout evidence\nCaptured:\n{}",
                ledger.capability,
                capture_context
            );
        }

        // JSONL log shows timeout flag.
        let log = build_log(&ledgers, fixture.name);
        for entry in &log {
            assert!(
                entry.timeout,
                "{} should have timeout flag",
                entry.capability
            );
        }
    }

    #[test]
    fn e2e_recorded_minimal_terminal() {
        let fixture = &MINIMAL_VT100_FIXTURE;
        log_capture(fixture);
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();

        fixture.feed(&mut prober);
        let ledgers = prober.build_ledgers(&caps);
        let capture_context = fixture.capture_context();

        // VT100 doesn't support modern features. SyncOutput and FocusEvents should be denied.
        let so = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::SynchronizedOutput)
            .unwrap();
        assert!(
            so.probability() < 0.1,
            "VT100 SyncOutput should be denied: {:.2}\nCaptured:\n{}",
            so.probability(),
            capture_context
        );
    }

    #[test]
    fn property_probe_ordering_independent() {
        // Feed DA2 before DA1 vs DA1 before DA2 → same result.
        let da1 = b"\x1b[?1;2;4;6;22c";
        let da2 = b"\x1b[>41;354;0c";
        let decrpm_sync = b"\x1b[?2026;1$y";
        let decrpm_focus = b"\x1b[?1004;1$y";

        let caps = TerminalCapabilities::basic();

        // Order A: DA1, DA2, DECRPM
        let mut prober_a = CapabilityProber::new(Duration::from_millis(200));
        prober_a.process_response(da1);
        prober_a.process_response(da2);
        prober_a.process_response(decrpm_sync);
        prober_a.process_response(decrpm_focus);
        let ledgers_a = prober_a.build_ledgers(&caps);

        // Order B: DECRPM, DA2, DA1
        let mut prober_b = CapabilityProber::new(Duration::from_millis(200));
        prober_b.process_response(decrpm_focus);
        prober_b.process_response(decrpm_sync);
        prober_b.process_response(da2);
        prober_b.process_response(da1);
        let ledgers_b = prober_b.build_ledgers(&caps);

        // Order C: DA2, DECRPM, DA1
        let mut prober_c = CapabilityProber::new(Duration::from_millis(200));
        prober_c.process_response(da2);
        prober_c.process_response(decrpm_sync);
        prober_c.process_response(decrpm_focus);
        prober_c.process_response(da1);
        let ledgers_c = prober_c.build_ledgers(&caps);

        // All orderings should produce the same decisions.
        for i in 0..ledgers_a.len() {
            let pa = ledgers_a[i].probability();
            let pb = ledgers_b[i].probability();
            let pc = ledgers_c[i].probability();

            assert!(
                (pa - pb).abs() < 1e-10 && (pb - pc).abs() < 1e-10,
                "{:?}: ordering matters! A={:.4}, B={:.4}, C={:.4}",
                ledgers_a[i].capability,
                pa,
                pb,
                pc,
            );
        }
    }

    #[test]
    fn property_probe_ordering_many_permutations() {
        // Test 6 permutations of 3 response types.
        let responses: [&[u8]; 3] = [
            b"\x1b[?1;2;4c",    // DA1
            b"\x1b[>41;354;0c", // DA2
            b"\x1b[?2026;1$y",  // DECRPM
        ];
        let caps = TerminalCapabilities::basic();

        let permutations: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];

        let mut reference: Option<Vec<f64>> = None;

        for perm in &permutations {
            let mut prober = CapabilityProber::new(Duration::from_millis(200));
            for &idx in perm {
                prober.process_response(responses[idx]);
            }
            let ledgers = prober.build_ledgers(&caps);
            let probs: Vec<f64> = ledgers.iter().map(|l| l.probability()).collect();

            if let Some(ref r) = reference {
                for (i, (&a, &b)) in r.iter().zip(probs.iter()).enumerate() {
                    assert!(
                        (a - b).abs() < 1e-10,
                        "Permutation {:?}: cap {} differs: {:.4} vs {:.4}",
                        perm,
                        i,
                        a,
                        b,
                    );
                }
            } else {
                reference = Some(probs);
            }
        }
    }

    #[test]
    fn e2e_recorded_mintty() {
        let fixture = &MINTTY_FIXTURE;
        log_capture(fixture);
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();

        fixture.feed(&mut prober);
        let ledgers = prober.build_ledgers(&caps);
        let capture_context = fixture.capture_context();

        let tc = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::TrueColor)
            .unwrap();
        assert!(
            tc.confident_at(0.8),
            "mintty TrueColor: {:.2}\nCaptured:\n{}",
            tc.probability(),
            capture_context
        );

        let hl = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::Hyperlinks)
            .unwrap();
        assert!(
            hl.confident_at(0.8),
            "mintty Hyperlinks: {:.2}\nCaptured:\n{}",
            hl.probability(),
            capture_context
        );

        let so = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::SynchronizedOutput)
            .unwrap();
        assert!(
            so.confident_at(0.8),
            "mintty SyncOutput: {:.2}\nCaptured:\n{}",
            so.probability(),
            capture_context
        );
    }

    #[test]
    fn e2e_jsonl_schema_valid() {
        // Validate that all JSONL fields are present.
        let fixture = &XTERM_FIXTURE;
        log_capture(fixture);
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        let caps = TerminalCapabilities::basic();
        fixture.feed(&mut prober);
        let ledgers = prober.build_ledgers(&caps);

        let log = build_log(&ledgers, fixture.name);
        for entry in &log {
            let line = entry.to_jsonl();
            assert!(line.contains("\"capability\":"));
            assert!(line.contains("\"probe\":"));
            assert!(line.contains("\"response\":"));
            assert!(line.contains("\"decision\":"));
            assert!(line.contains("\"confidence\":"));
            assert!(line.contains("\"timeout\":"));
        }
    }

    #[test]
    fn e2e_env_plus_probe_stacking() {
        // When env detection + probe confirmation agree, confidence should be very high.
        let fixture = &XTERM_FIXTURE;
        log_capture(fixture);
        let mut prober = CapabilityProber::new(Duration::from_millis(200));
        fixture.feed(&mut prober);

        let mut caps = TerminalCapabilities::basic();
        caps.true_color = true; // env says true
        caps.osc8_hyperlinks = true;

        let ledgers = prober.build_ledgers(&caps);
        let tc = ledgers
            .iter()
            .find(|l| l.capability == ProbeableCapability::TrueColor)
            .unwrap();

        // Both env + DA2 confirm → very high confidence.
        assert!(
            tc.probability() > 0.98,
            "Stacked evidence: {:.4}\nCaptured:\n{}",
            tc.probability(),
            fixture.capture_context()
        );
    }
}
