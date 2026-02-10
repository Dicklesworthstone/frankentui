#![forbid(unsafe_code)]

//! Terminal Capability Explorer screen.
//!
//! Visualizes terminal capability detection, safety policies, and evidence
//! ledgers from `ftui-core`. Provides a simple simulation mode to preview
//! predefined terminal profiles.
//!
//! ## Diagnostic Logging
//!
//! Set `FTUI_TERMCAPS_DIAGNOSTICS=true` to enable JSONL diagnostic output to stderr.
//! Events logged include view mode changes, profile cycling, capability selection,
//! and evidence ledger access.
//!
//! For deterministic timestamps in tests, set `FTUI_TERMCAPS_DETERMINISTIC=true`.

use std::cell::Cell;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ftui_core::capability_override::{has_active_overrides, override_depth};
#[cfg(feature = "caps-probe")]
use ftui_core::caps_probe::{CapabilityProber, EvidenceSource, ProbeableCapability};
use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::Text;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};
use serde_json::json;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::theme;

#[cfg(not(feature = "caps-probe"))]
mod caps_probe_stub {
    use ftui_core::terminal_capabilities::TerminalCapabilities;
    use std::time::Duration;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum EvidenceSource {
        Environment,
        Da1Response,
        Da2Response,
        DecrpmResponse,
        OscResponse,
        Timeout,
        Prior,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum ProbeableCapability {
        TrueColor,
        SynchronizedOutput,
        Hyperlinks,
        KittyKeyboard,
        FocusEvents,
    }

    #[derive(Debug, Clone)]
    pub struct EvidenceEntry {
        pub source: EvidenceSource,
        pub log_odds: f64,
    }

    #[derive(Debug, Clone)]
    pub struct EvidenceLedger {
        pub capability: ProbeableCapability,
        entries: Vec<EvidenceEntry>,
        probability: f64,
        log_odds: f64,
    }

    impl EvidenceLedger {
        pub fn probability(&self) -> f64 {
            self.probability
        }

        pub fn is_supported(&self) -> bool {
            self.probability >= 0.5
        }

        pub fn log_odds(&self) -> f64 {
            self.log_odds
        }

        pub fn entries(&self) -> &[EvidenceEntry] {
            &self.entries
        }
    }

    #[derive(Debug, Clone)]
    pub struct CapabilityProber;

    impl CapabilityProber {
        pub fn new(_timeout: Duration) -> Self {
            Self
        }

        pub fn build_ledgers(&self, _caps: &TerminalCapabilities) -> Vec<EvidenceLedger> {
            Vec::new()
        }
    }
}

#[cfg(not(feature = "caps-probe"))]
use caps_probe_stub::{CapabilityProber, EvidenceSource, ProbeableCapability};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Matrix,
    Evidence,
    Simulation,
}

impl ViewMode {
    fn next(self) -> Self {
        match self {
            Self::Matrix => Self::Evidence,
            Self::Evidence => Self::Simulation,
            Self::Simulation => Self::Matrix,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Matrix => "Matrix",
            Self::Evidence => "Evidence",
            Self::Simulation => "Simulation",
        }
    }
}

// ============================================================================
// Diagnostic Logging Infrastructure
// ============================================================================

/// Counter for generating unique diagnostic entry IDs.
static DIAGNOSTIC_SEQ: AtomicU64 = AtomicU64::new(0);

/// Reset the diagnostic sequence counter (for testing).
pub fn reset_diagnostic_seq() {
    DIAGNOSTIC_SEQ.store(0, Ordering::Relaxed);
}

/// Types of diagnostic events emitted by the Terminal Capabilities screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEventKind {
    /// View mode changed (Tab pressed).
    ViewModeChanged,
    /// Capability selection moved (Up/Down).
    SelectionChanged,
    /// Profile was cycled (P key).
    ProfileCycled,
    /// Profile was reset to detected (R key).
    ProfileReset,
    /// Capability row was inspected (selection landed on it).
    CapabilityInspected,
    /// Evidence ledger was accessed (Evidence view shown).
    EvidenceLedgerAccessed,
    /// Simulation mode activated.
    SimulationActivated,
    /// Environment snapshot was read.
    EnvironmentRead,
    /// Capability report exported to JSONL.
    ReportExported,
}

impl DiagnosticEventKind {
    /// Returns a stable string label for JSONL output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ViewModeChanged => "view_mode_changed",
            Self::SelectionChanged => "selection_changed",
            Self::ProfileCycled => "profile_cycled",
            Self::ProfileReset => "profile_reset",
            Self::CapabilityInspected => "capability_inspected",
            Self::EvidenceLedgerAccessed => "evidence_ledger_accessed",
            Self::SimulationActivated => "simulation_activated",
            Self::EnvironmentRead => "environment_read",
            Self::ReportExported => "report_exported",
        }
    }
}

/// A single diagnostic log entry.
#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    /// Unique sequence ID for this entry.
    pub seq: u64,
    /// Event kind.
    pub kind: DiagnosticEventKind,
    /// Unix timestamp in milliseconds (or deterministic counter in test mode).
    pub timestamp_ms: u64,
    /// Optional key-value details.
    details: Vec<(String, String)>,
}

impl DiagnosticEntry {
    /// Create a new diagnostic entry with auto-assigned sequence ID.
    pub fn new(kind: DiagnosticEventKind) -> Self {
        let seq = DIAGNOSTIC_SEQ.fetch_add(1, Ordering::Relaxed);
        let timestamp_ms = if determinism::env_flag("FTUI_TERMCAPS_DETERMINISTIC")
            || determinism::is_demo_deterministic()
        {
            seq // Use sequence as deterministic timestamp
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        };
        Self {
            seq,
            kind,
            timestamp_ms,
            details: Vec::new(),
        }
    }

    /// Add a detail key-value pair (builder pattern).
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: &str) -> Self {
        self.details.push((key.to_string(), value.to_string()));
        self
    }

    /// Add view mode detail.
    #[must_use]
    pub fn with_view_mode(self, mode: ViewMode) -> Self {
        self.with_detail("view_mode", mode.label())
    }

    /// Add selected capability detail.
    #[must_use]
    pub fn with_capability(self, name: &str) -> Self {
        self.with_detail("capability", name)
    }

    /// Add profile detail.
    #[must_use]
    pub fn with_profile(self, profile: &str) -> Self {
        self.with_detail("profile", profile)
    }

    /// Add selection index detail.
    #[must_use]
    pub fn with_selection(self, index: usize) -> Self {
        self.with_detail("selection", &index.to_string())
    }

    /// Escape a string value for JSONL output.
    fn escape_json(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                c if c.is_control() => {
                    result.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => result.push(c),
            }
        }
        result
    }

    /// Serialize to JSONL format.
    pub fn to_jsonl(&self) -> String {
        let mut parts = vec![
            format!("\"seq\":{}", self.seq),
            format!("\"ts\":{}", self.timestamp_ms),
            format!("\"kind\":\"{}\"", self.kind.as_str()),
        ];
        for (key, value) in &self.details {
            parts.push(format!(
                "\"{}\":\"{}\"",
                Self::escape_json(key),
                Self::escape_json(value)
            ));
        }
        format!("{{{}}}", parts.join(","))
    }
}

/// Summary of diagnostic events for reporting.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticSummary {
    pub view_mode_changed_count: usize,
    pub selection_changed_count: usize,
    pub profile_cycled_count: usize,
    pub profile_reset_count: usize,
    pub capability_inspected_count: usize,
    pub evidence_ledger_accessed_count: usize,
    pub simulation_activated_count: usize,
    pub environment_read_count: usize,
    pub report_exported_count: usize,
}

impl DiagnosticSummary {
    /// Total number of events.
    pub fn total(&self) -> usize {
        self.view_mode_changed_count
            + self.selection_changed_count
            + self.profile_cycled_count
            + self.profile_reset_count
            + self.capability_inspected_count
            + self.evidence_ledger_accessed_count
            + self.simulation_activated_count
            + self.environment_read_count
            + self.report_exported_count
    }

    /// Serialize to JSONL format.
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"summary\":true,\"total\":{},\"view_mode_changed\":{},\"selection_changed\":{},\"profile_cycled\":{},\"profile_reset\":{},\"capability_inspected\":{},\"evidence_ledger_accessed\":{},\"simulation_activated\":{},\"environment_read\":{},\"report_exported\":{}}}",
            self.total(),
            self.view_mode_changed_count,
            self.selection_changed_count,
            self.profile_cycled_count,
            self.profile_reset_count,
            self.capability_inspected_count,
            self.evidence_ledger_accessed_count,
            self.simulation_activated_count,
            self.environment_read_count,
            self.report_exported_count
        )
    }
}

/// Collector for diagnostic entries.
#[derive(Debug)]
pub struct DiagnosticLog {
    entries: Vec<DiagnosticEntry>,
    max_entries: usize,
    write_to_stderr: bool,
    start_time: Instant,
}

impl DiagnosticLog {
    /// Create a new diagnostic log.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 10_000,
            write_to_stderr: false,
            start_time: Instant::now(),
        }
    }

    /// Enable writing entries to stderr as JSONL.
    #[must_use]
    pub fn with_stderr(mut self) -> Self {
        self.write_to_stderr = true;
        self
    }

    /// Set maximum number of entries to retain.
    #[must_use]
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max;
        self
    }

    /// Record a diagnostic entry.
    pub fn record(&mut self, entry: DiagnosticEntry) {
        if self.write_to_stderr {
            let _ = writeln!(std::io::stderr(), "{}", entry.to_jsonl());
        }
        self.entries.push(entry);
        if self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
    }

    /// Get all recorded entries.
    pub fn entries(&self) -> &[DiagnosticEntry] {
        &self.entries
    }

    /// Get entries of a specific kind.
    pub fn entries_of_kind(&self, kind: DiagnosticEventKind) -> Vec<&DiagnosticEntry> {
        self.entries.iter().filter(|e| e.kind == kind).collect()
    }

    /// Export all entries as JSONL.
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .map(DiagnosticEntry::to_jsonl)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Compute summary statistics.
    pub fn summary(&self) -> DiagnosticSummary {
        let mut summary = DiagnosticSummary::default();
        for entry in &self.entries {
            match entry.kind {
                DiagnosticEventKind::ViewModeChanged => summary.view_mode_changed_count += 1,
                DiagnosticEventKind::SelectionChanged => summary.selection_changed_count += 1,
                DiagnosticEventKind::ProfileCycled => summary.profile_cycled_count += 1,
                DiagnosticEventKind::ProfileReset => summary.profile_reset_count += 1,
                DiagnosticEventKind::CapabilityInspected => summary.capability_inspected_count += 1,
                DiagnosticEventKind::EvidenceLedgerAccessed => {
                    summary.evidence_ledger_accessed_count += 1
                }
                DiagnosticEventKind::SimulationActivated => summary.simulation_activated_count += 1,
                DiagnosticEventKind::EnvironmentRead => summary.environment_read_count += 1,
                DiagnosticEventKind::ReportExported => summary.report_exported_count += 1,
            }
        }
        summary
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Elapsed time since log creation.
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }
}

impl Default for DiagnosticLog {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// End Diagnostic Logging Infrastructure
// ============================================================================

#[derive(Debug, Clone)]
struct CapabilityRow {
    name: &'static str,
    detected: bool,
    effective: bool,
    fallback: String,
    reason: String,
    probeable: Option<ProbeableCapability>,
}

#[derive(Debug, Clone)]
struct ComparisonRow {
    name: &'static str,
    detected: bool,
    simulated: bool,
}

#[derive(Debug, Clone)]
struct ReportStatus {
    path: String,
    success: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    term: String,
    term_program: String,
    colorterm: String,
    no_color: bool,
    tmux: bool,
    screen: bool,
    zellij: bool,
    kitty: bool,
    wt_session: bool,
}

impl EnvSnapshot {
    pub fn read() -> Self {
        Self {
            term: std::env::var("TERM").unwrap_or_default(),
            term_program: std::env::var("TERM_PROGRAM").unwrap_or_default(),
            colorterm: std::env::var("COLORTERM").unwrap_or_default(),
            no_color: std::env::var("NO_COLOR").is_ok(),
            tmux: std::env::var("TMUX").is_ok(),
            screen: std::env::var("STY").is_ok(),
            zellij: std::env::var("ZELLIJ").is_ok(),
            kitty: std::env::var("KITTY_WINDOW_ID").is_ok(),
            wt_session: std::env::var("WT_SESSION").is_ok(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_values(
        term: &str,
        term_program: &str,
        colorterm: &str,
        no_color: bool,
        tmux: bool,
        screen: bool,
        zellij: bool,
        kitty: bool,
        wt_session: bool,
    ) -> Self {
        Self {
            term: term.to_string(),
            term_program: term_program.to_string(),
            colorterm: colorterm.to_string(),
            no_color,
            tmux,
            screen,
            zellij,
            kitty,
            wt_session,
        }
    }

    fn format_value(value: &str) -> String {
        if value.is_empty() {
            "(unset)".to_string()
        } else {
            value.to_string()
        }
    }
}

pub struct TerminalCapabilitiesScreen {
    view: ViewMode,
    selected: usize,
    profile_override: Option<TerminalProfile>,
    detected_profile_override: Option<TerminalProfile>,
    detected_caps_override: Option<TerminalCapabilities>,
    prober: CapabilityProber,
    env_override: Option<EnvSnapshot>,
    diagnostic_log: DiagnosticLog,
    last_report: Option<ReportStatus>,
    tick_count: u64,
    /// Cached render area of the matrix panel for mouse hit-testing.
    last_matrix_area: Cell<Rect>,
}

impl Default for TerminalCapabilitiesScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalCapabilitiesScreen {
    pub fn new() -> Self {
        let diagnostic_log = if std::env::var("FTUI_TERMCAPS_DIAGNOSTICS").is_ok() {
            DiagnosticLog::new().with_stderr()
        } else {
            DiagnosticLog::new()
        };
        Self {
            view: ViewMode::Matrix,
            selected: 0,
            profile_override: None,
            detected_profile_override: None,
            detected_caps_override: None,
            prober: CapabilityProber::new(Duration::from_millis(200)),
            env_override: None,
            diagnostic_log,
            last_report: None,
            tick_count: 0,
            last_matrix_area: Cell::new(Rect::default()),
        }
    }

    /// Get a reference to the diagnostic log.
    pub fn diagnostic_log(&self) -> &DiagnosticLog {
        &self.diagnostic_log
    }

    /// Get a mutable reference to the diagnostic log.
    pub fn diagnostic_log_mut(&mut self) -> &mut DiagnosticLog {
        &mut self.diagnostic_log
    }

    pub fn with_profile(profile: TerminalProfile) -> Self {
        let mut screen = Self::new();
        screen.set_profile_override(profile);
        screen
    }

    pub fn set_profile_override(&mut self, profile: TerminalProfile) {
        if profile == TerminalProfile::Detected {
            self.profile_override = None;
        } else {
            self.profile_override = Some(profile);
        }
    }

    pub fn set_detected_profile_override(&mut self, profile: TerminalProfile) {
        if profile == TerminalProfile::Detected {
            self.detected_profile_override = None;
        } else {
            self.detected_profile_override = Some(profile);
        }
    }

    pub fn set_detected_capabilities_override(&mut self, caps: TerminalCapabilities) {
        self.detected_caps_override = Some(caps);
    }

    pub fn set_env_override(&mut self, env: EnvSnapshot) {
        self.env_override = Some(env);
    }

    fn detected_capabilities(&self) -> TerminalCapabilities {
        if let Some(caps) = self.detected_caps_override {
            return caps;
        }
        match self.detected_profile_override {
            Some(profile) => TerminalCapabilities::from_profile(profile),
            None => TerminalCapabilities::with_overrides(),
        }
    }

    fn simulated_capabilities(&self, detected: &TerminalCapabilities) -> TerminalCapabilities {
        match self.profile_override {
            Some(profile) => TerminalCapabilities::from_profile(profile),
            None => *detected,
        }
    }

    fn active_capabilities(&self, detected: &TerminalCapabilities) -> TerminalCapabilities {
        self.simulated_capabilities(detected)
    }

    fn profile_label(profile: TerminalProfile) -> &'static str {
        if profile == TerminalProfile::Detected {
            "detected"
        } else {
            profile.as_str()
        }
    }

    fn status_text(enabled: bool) -> Text {
        if enabled {
            Text::styled("yes", theme::success())
        } else {
            Text::styled("no", theme::error_style())
        }
    }

    fn fallback_text(enabled: bool, fallback: &str) -> String {
        if enabled {
            "enabled".to_string()
        } else {
            fallback.to_string()
        }
    }

    fn reason_for(
        caps: &TerminalCapabilities,
        detected: bool,
        effective: bool,
        mux_sensitive: bool,
    ) -> String {
        if detected && effective {
            "Detected via env heuristics".to_string()
        } else if mux_sensitive && caps.in_any_mux() {
            "Disabled by mux safety policy".to_string()
        } else if detected && !effective {
            "Policy disabled (safety)".to_string()
        } else {
            "Conservative default (insufficient evidence)".to_string()
        }
    }

    fn build_rows(&self, caps: &TerminalCapabilities) -> Vec<CapabilityRow> {
        vec![
            CapabilityRow {
                name: "True color (24-bit)",
                detected: caps.true_color,
                effective: caps.true_color,
                fallback: Self::fallback_text(
                    caps.true_color,
                    if caps.colors_256 { "256-color" } else { "mono" },
                ),
                reason: Self::reason_for(caps, caps.true_color, caps.true_color, false),
                probeable: Some(ProbeableCapability::TrueColor),
            },
            CapabilityRow {
                name: "256-color palette",
                detected: caps.colors_256,
                effective: caps.colors_256,
                fallback: Self::fallback_text(caps.colors_256, "mono"),
                reason: Self::reason_for(caps, caps.colors_256, caps.colors_256, false),
                probeable: None,
            },
            CapabilityRow {
                name: "Synchronized output",
                detected: caps.sync_output,
                effective: caps.use_sync_output(),
                fallback: Self::fallback_text(caps.use_sync_output(), "unsynced repaint"),
                reason: Self::reason_for(caps, caps.sync_output, caps.use_sync_output(), true),
                probeable: Some(ProbeableCapability::SynchronizedOutput),
            },
            CapabilityRow {
                name: "Scroll region (DECSTBM)",
                detected: caps.scroll_region,
                effective: caps.use_scroll_region(),
                fallback: Self::fallback_text(caps.use_scroll_region(), "full repaint"),
                reason: Self::reason_for(caps, caps.scroll_region, caps.use_scroll_region(), true),
                probeable: None,
            },
            CapabilityRow {
                name: "OSC 8 hyperlinks",
                detected: caps.osc8_hyperlinks,
                effective: caps.use_hyperlinks(),
                fallback: Self::fallback_text(caps.use_hyperlinks(), "plain text"),
                reason: Self::reason_for(caps, caps.osc8_hyperlinks, caps.use_hyperlinks(), true),
                probeable: Some(ProbeableCapability::Hyperlinks),
            },
            CapabilityRow {
                name: "Kitty keyboard protocol",
                detected: caps.kitty_keyboard,
                effective: caps.kitty_keyboard,
                fallback: Self::fallback_text(caps.kitty_keyboard, "legacy keys"),
                reason: Self::reason_for(caps, caps.kitty_keyboard, caps.kitty_keyboard, false),
                probeable: Some(ProbeableCapability::KittyKeyboard),
            },
            CapabilityRow {
                name: "Focus events",
                detected: caps.focus_events,
                effective: caps.focus_events,
                fallback: Self::fallback_text(caps.focus_events, "no focus events"),
                reason: Self::reason_for(caps, caps.focus_events, caps.focus_events, false),
                probeable: Some(ProbeableCapability::FocusEvents),
            },
            CapabilityRow {
                name: "Bracketed paste",
                detected: caps.bracketed_paste,
                effective: caps.bracketed_paste,
                fallback: Self::fallback_text(caps.bracketed_paste, "raw paste"),
                reason: Self::reason_for(caps, caps.bracketed_paste, caps.bracketed_paste, false),
                probeable: None,
            },
            CapabilityRow {
                name: "SGR mouse",
                detected: caps.mouse_sgr,
                effective: caps.mouse_sgr,
                fallback: Self::fallback_text(caps.mouse_sgr, "mouse disabled"),
                reason: Self::reason_for(caps, caps.mouse_sgr, caps.mouse_sgr, false),
                probeable: None,
            },
            CapabilityRow {
                name: "OSC 52 clipboard",
                detected: caps.osc52_clipboard,
                effective: caps.use_clipboard(),
                fallback: Self::fallback_text(caps.use_clipboard(), "clipboard disabled"),
                reason: Self::reason_for(caps, caps.osc52_clipboard, caps.use_clipboard(), true),
                probeable: None,
            },
        ]
    }

    fn build_comparison_rows(
        &self,
        detected: &TerminalCapabilities,
        simulated: &TerminalCapabilities,
    ) -> Vec<ComparisonRow> {
        vec![
            ComparisonRow {
                name: "True color (24-bit)",
                detected: detected.true_color,
                simulated: simulated.true_color,
            },
            ComparisonRow {
                name: "256-color palette",
                detected: detected.colors_256,
                simulated: simulated.colors_256,
            },
            ComparisonRow {
                name: "Synchronized output",
                detected: detected.sync_output,
                simulated: simulated.sync_output,
            },
            ComparisonRow {
                name: "Scroll region (DECSTBM)",
                detected: detected.scroll_region,
                simulated: simulated.scroll_region,
            },
            ComparisonRow {
                name: "OSC 8 hyperlinks",
                detected: detected.osc8_hyperlinks,
                simulated: simulated.osc8_hyperlinks,
            },
            ComparisonRow {
                name: "Kitty keyboard protocol",
                detected: detected.kitty_keyboard,
                simulated: simulated.kitty_keyboard,
            },
            ComparisonRow {
                name: "Focus events",
                detected: detected.focus_events,
                simulated: simulated.focus_events,
            },
            ComparisonRow {
                name: "Bracketed paste",
                detected: detected.bracketed_paste,
                simulated: simulated.bracketed_paste,
            },
            ComparisonRow {
                name: "SGR mouse",
                detected: detected.mouse_sgr,
                simulated: simulated.mouse_sgr,
            },
            ComparisonRow {
                name: "OSC 52 clipboard",
                detected: detected.osc52_clipboard,
                simulated: simulated.osc52_clipboard,
            },
        ]
    }

    fn selected_row<'a>(&self, rows: &'a [CapabilityRow]) -> Option<&'a CapabilityRow> {
        rows.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize, row_count: usize) {
        if row_count == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected as isize;
        let mut next = current + delta;
        if next < 0 {
            next = row_count as isize - 1;
        } else if next >= row_count as isize {
            next = 0;
        }
        self.selected = next as usize;
    }

    fn cycle_profile(&mut self) {
        let profiles = Self::profile_order();
        let current = self.profile_override.unwrap_or(TerminalProfile::Detected);
        let idx = profiles.iter().position(|&p| p == current).unwrap_or(0);
        let next = profiles[(idx + 1) % profiles.len()];
        self.profile_override = if next == TerminalProfile::Detected {
            None
        } else {
            Some(next)
        };
    }

    fn reset_profile(&mut self) {
        self.profile_override = None;
    }

    /// Jump directly to a profile by 1-based index into the profile order.
    /// Index 0 is "Detected" (no override), 1..N map to the non-detected
    /// entries of [`profile_order`].
    fn set_profile_by_index(&mut self, index: usize) {
        let profiles = Self::profile_order();
        if let Some(&profile) = profiles.get(index) {
            self.profile_override = if profile == TerminalProfile::Detected {
                None
            } else {
                Some(profile)
            };
        }
    }

    /// Handle mouse events over the matrix panel area.
    fn handle_mouse(&mut self, mouse: &MouseEvent) {
        let matrix_area = self.last_matrix_area.get();
        if matrix_area.is_empty() {
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if matrix_area.contains(mouse.x, mouse.y) => {
                // +2 accounts for the border (1 row) and header (1 row).
                let header_offset = 2u16;
                if mouse.y >= matrix_area.y + header_offset {
                    let row_idx = (mouse.y - matrix_area.y - header_offset) as usize;
                    let detected = self.detected_capabilities();
                    let active = self.active_capabilities(&detected);
                    let rows = self.build_rows(&active);
                    if row_idx < rows.len() {
                        let old_selected = self.selected;
                        self.selected = row_idx;
                        if self.selected != old_selected {
                            let entry = DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged)
                                .with_selection(self.selected)
                                .with_detail("from", &old_selected.to_string())
                                .with_detail("via", "mouse");
                            self.diagnostic_log.record(entry);
                            if let Some(row) = rows.get(self.selected) {
                                let entry =
                                    DiagnosticEntry::new(DiagnosticEventKind::CapabilityInspected)
                                        .with_capability(row.name)
                                        .with_detail("detected", &row.detected.to_string())
                                        .with_detail("effective", &row.effective.to_string());
                                self.diagnostic_log.record(entry);
                            }
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp if matrix_area.contains(mouse.x, mouse.y) => {
                let detected = self.detected_capabilities();
                let active = self.active_capabilities(&detected);
                let rows = self.build_rows(&active);
                self.move_selection(-1, rows.len());
            }
            MouseEventKind::ScrollDown if matrix_area.contains(mouse.x, mouse.y) => {
                let detected = self.detected_capabilities();
                let active = self.active_capabilities(&detected);
                let rows = self.build_rows(&active);
                self.move_selection(1, rows.len());
            }
            _ => {}
        }
    }

    fn profile_order() -> &'static [TerminalProfile] {
        &[
            TerminalProfile::Detected,
            TerminalProfile::Modern,
            TerminalProfile::Xterm256Color,
            TerminalProfile::Xterm,
            TerminalProfile::Vt100,
            TerminalProfile::Dumb,
            TerminalProfile::Tmux,
            TerminalProfile::Screen,
            TerminalProfile::Zellij,
            TerminalProfile::Kitty,
            TerminalProfile::WindowsConsole,
            TerminalProfile::LinuxConsole,
        ]
    }

    fn render_summary(
        &self,
        frame: &mut Frame,
        area: Rect,
        detected: &TerminalCapabilities,
        active: &TerminalCapabilities,
    ) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Terminal Capability Explorer ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let detected_profile = detected.profile();
        let simulated_profile = self.profile_override.unwrap_or(detected_profile);
        let detected_label = Self::profile_label(detected_profile);
        let simulated_label = Self::profile_label(simulated_profile);
        let mux_state = if active.in_any_mux() {
            let mut muxes = Vec::new();
            if active.in_tmux {
                muxes.push("tmux");
            }
            if active.in_screen {
                muxes.push("screen");
            }
            if active.in_zellij {
                muxes.push("zellij");
            }
            if muxes.is_empty() {
                "yes".to_string()
            } else {
                format!("yes ({})", muxes.join(", "))
            }
        } else {
            "no".to_string()
        };

        let overrides = if has_active_overrides() {
            format!("overrides: {} active", override_depth())
        } else {
            "overrides: none".to_string()
        };

        let profile_line = if self.profile_override.is_some() {
            format!(
                "Detected: {} | Simulated: {} | View: {}",
                detected_label,
                simulated_label,
                self.view.label()
            )
        } else {
            format!("Profile: {} | View: {}", detected_label, self.view.label())
        };

        let lines = [
            profile_line,
            format!(
                "Color depth: {} | Mux: {} | Sync output: {}",
                active.color_depth(),
                mux_state,
                yes_no(active.use_sync_output())
            ),
            format!(
                "Scroll region: {} | Hyperlinks: {} | Clipboard: {} | {}",
                yes_no(active.use_scroll_region()),
                yes_no(active.use_hyperlinks()),
                yes_no(active.use_clipboard()),
                overrides
            ),
        ];

        for (idx, line) in lines.iter().enumerate() {
            if idx as u16 >= inner.height {
                break;
            }
            let row = Rect::new(inner.x, inner.y + idx as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(row, frame);
        }
    }

    fn render_matrix_panel(&self, frame: &mut Frame, area: Rect, rows: &[CapabilityRow]) {
        self.last_matrix_area.set(area);
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Capability Matrix ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let header = Row::new(["Capability", "Detected", "Effective", "Fallback"])
            .style(Style::new().fg(theme::fg::PRIMARY).attrs(StyleFlags::BOLD));

        let highlight = Style::new()
            .bg(theme::alpha::HIGHLIGHT)
            .fg(theme::fg::PRIMARY)
            .attrs(StyleFlags::BOLD);

        let table_rows: Vec<Row> = rows
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let row_style = if idx == self.selected {
                    highlight
                } else {
                    Style::new().fg(theme::fg::SECONDARY)
                };
                Row::new([
                    Text::raw(row.name),
                    Self::status_text(row.detected),
                    Self::status_text(row.effective),
                    Text::raw(row.fallback.as_str()),
                ])
                .style(row_style)
            })
            .collect();

        let widths = [
            Constraint::Min(20),
            Constraint::Fixed(9),
            Constraint::Fixed(9),
            Constraint::Min(16),
        ];

        Widget::render(
            &Table::new(table_rows, widths)
                .header(header)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .theme(theme::table_theme_demo())
                .theme_phase(theme::table_theme_phase(self.tick_count)),
            inner,
            frame,
        );
    }

    fn render_comparison_panel(&self, frame: &mut Frame, area: Rect, rows: &[ComparisonRow]) {
        self.last_matrix_area.set(area);
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Detected vs Simulated ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let header = Row::new(["Capability", "Detected", "Simulated", "Δ"])
            .style(Style::new().fg(theme::fg::PRIMARY).attrs(StyleFlags::BOLD));

        let highlight = Style::new()
            .bg(theme::alpha::HIGHLIGHT)
            .fg(theme::fg::PRIMARY)
            .attrs(StyleFlags::BOLD);

        let table_rows: Vec<Row> = rows
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let mismatch = row.detected != row.simulated;
                let row_style = if idx == self.selected {
                    highlight
                } else if mismatch {
                    theme::warning()
                } else {
                    Style::new().fg(theme::fg::SECONDARY)
                };
                let delta = if mismatch { "!" } else { "" };
                Row::new([
                    Text::raw(row.name),
                    Self::status_text(row.detected),
                    Self::status_text(row.simulated),
                    Text::raw(delta),
                ])
                .style(row_style)
            })
            .collect();

        let widths = [
            Constraint::Min(20),
            Constraint::Fixed(9),
            Constraint::Fixed(9),
            Constraint::Fixed(2),
        ];

        Widget::render(
            &Table::new(table_rows, widths)
                .header(header)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .theme(theme::table_theme_demo())
                .theme_phase(theme::table_theme_phase(self.tick_count)),
            inner,
            frame,
        );
    }

    fn render_policy_panel(&self, frame: &mut Frame, area: Rect, caps: &TerminalCapabilities) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Fallback Policy ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let sync_reason =
            policy_reason(caps.sync_output, caps.use_sync_output(), caps.in_any_mux());
        let scroll_reason = policy_reason(
            caps.scroll_region,
            caps.use_scroll_region(),
            caps.in_any_mux(),
        );

        let lines = [
            format!(
                "Sync output: {} ({})",
                yes_no(caps.use_sync_output()),
                sync_reason
            ),
            format!(
                "Scroll region: {} ({})",
                yes_no(caps.use_scroll_region()),
                scroll_reason
            ),
            format!(
                "Mux safety: {}",
                if caps.in_any_mux() {
                    "active"
                } else {
                    "inactive"
                }
            ),
        ];

        for (idx, line) in lines.iter().enumerate() {
            if idx as u16 >= inner.height {
                break;
            }
            let row_area = Rect::new(inner.x, inner.y + idx as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::MUTED))
                .render(row_area, frame);
        }
    }

    fn render_details_panel(
        &self,
        frame: &mut Frame,
        area: Rect,
        row: &CapabilityRow,
        comparison: Option<&ComparisonRow>,
    ) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Details ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let (detected_line, simulated_line) = if let Some(comp) = comparison {
            (
                format!("Detected: {}", yes_no(comp.detected)),
                Some(format!("Simulated: {}", yes_no(comp.simulated))),
            )
        } else {
            (format!("Detected: {}", yes_no(row.detected)), None)
        };

        let mut lines = vec![
            format!("Capability: {}", row.name),
            detected_line,
            format!("Effective: {}", yes_no(row.effective)),
            format!("Fallback: {}", row.fallback),
            format!("Reason: {}", row.reason),
        ];

        if let Some(simulated) = simulated_line {
            lines.insert(2, simulated);
        }

        for (idx, line) in lines.iter().enumerate() {
            if idx as u16 >= inner.height {
                break;
            }
            let row_area = Rect::new(inner.x, inner.y + idx as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::MUTED))
                .render(row_area, frame);
        }
    }

    fn render_evidence_panel(
        &self,
        frame: &mut Frame,
        area: Rect,
        caps: &TerminalCapabilities,
        row: &CapabilityRow,
    ) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Evidence Ledger ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines: Vec<String> = Vec::new();
        if let Some(cap) = row.probeable {
            if let Some(ledger) = self
                .prober
                .build_ledgers(caps)
                .into_iter()
                .find(|l| l.capability == cap)
            {
                let percent: f64 = (ledger.probability() * 100.0).round();
                let decision = if ledger.is_supported() {
                    "likely supported"
                } else {
                    "unlikely supported"
                };
                lines.push(format!("Capability: {} ({})", row.name, decision));
                lines.push(format!(
                    "Confidence: {}% | log-odds {:.2}",
                    percent,
                    ledger.log_odds()
                ));
                lines.push("Evidence:".to_string());
                for entry in ledger.entries() {
                    lines.push(format!(
                        "  {:<14} {:+.2}",
                        evidence_label(entry.source),
                        entry.log_odds
                    ));
                }
            } else {
                lines.push("No evidence ledger available.".to_string());
            }
        } else {
            lines.push(format!("Capability: {}", row.name));
            lines.push("Not probeable (env heuristics only).".to_string());
        }

        for (idx, line) in lines.iter().enumerate() {
            if idx as u16 >= inner.height {
                break;
            }
            let row_area = Rect::new(inner.x, inner.y + idx as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(row_area, frame);
        }
    }

    fn render_simulation_panel(&self, frame: &mut Frame, area: Rect, caps: &TerminalCapabilities) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Profile Simulation ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let profiles = Self::profile_order();
        let active = self.profile_override.unwrap_or(TerminalProfile::Detected);
        let badge_label = if self.profile_override.is_some() {
            "Simulated profile"
        } else {
            "Detected profile"
        };
        let mut row_idx = 0u16;

        if inner.height > 0 {
            let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
            Paragraph::new(format!("{}: {}", badge_label, Self::profile_label(active)))
                .style(
                    Style::new()
                        .fg(theme::screen_accent::ADVANCED)
                        .attrs(StyleFlags::BOLD),
                )
                .render(header_area, frame);
            row_idx = row_idx.saturating_add(1);
        }

        for profile in profiles {
            if row_idx >= inner.height {
                break;
            }
            let label = Self::profile_label(*profile);
            let prefix = if *profile == active { "> " } else { "  " };
            let line = format!("{prefix}{label}");
            let style = if *profile == active {
                Style::new().fg(theme::fg::PRIMARY).attrs(StyleFlags::BOLD)
            } else {
                Style::new().fg(theme::fg::MUTED)
            };
            let row_area = Rect::new(inner.x, inner.y + row_idx, inner.width, 1);
            Paragraph::new(line).style(style).render(row_area, frame);
            row_idx = row_idx.saturating_add(1);
        }

        if row_idx < inner.height {
            let hint_area = Rect::new(inner.x, inner.y + row_idx, inner.width, 1);
            Paragraph::new("P: cycle profiles | R: reset to detected")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(hint_area, frame);
        }

        let mut footer_lines = vec![format!(
            "Preview: color={} sync={} mux={}",
            caps.color_depth(),
            yes_no(caps.use_sync_output()),
            yes_no(caps.in_any_mux())
        )];

        if let Some(report) = &self.last_report {
            let line = if report.success {
                format!("Last report: {}", report.path)
            } else {
                format!(
                    "Report failed: {}",
                    report.error.as_deref().unwrap_or("unknown error")
                )
            };
            footer_lines.push(line);
        }

        let footer_lines = footer_lines
            .into_iter()
            .take(inner.height as usize)
            .collect::<Vec<_>>();
        let footer_len = footer_lines.len() as u16;
        let start_y = inner.bottom().saturating_sub(footer_len);

        for (idx, line) in footer_lines.iter().enumerate() {
            let row_area = Rect::new(inner.x, start_y + idx as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(row_area, frame);
        }
    }

    fn render_environment_panel(
        &self,
        frame: &mut Frame,
        area: Rect,
        env: &EnvSnapshot,
        caps: &TerminalCapabilities,
    ) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Environment ")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let lines = [
            format!("TERM={}", EnvSnapshot::format_value(&env.term)),
            format!(
                "TERM_PROGRAM={}",
                EnvSnapshot::format_value(&env.term_program)
            ),
            format!("COLORTERM={}", EnvSnapshot::format_value(&env.colorterm)),
            format!("NO_COLOR={}", yes_no(env.no_color)),
            format!(
                "MUX: tmux={} screen={} zellij={}",
                yes_no(env.tmux),
                yes_no(env.screen),
                yes_no(env.zellij)
            ),
            format!(
                "KITTY_WINDOW_ID={} WT_SESSION={}",
                yes_no(env.kitty),
                yes_no(env.wt_session)
            ),
            format!(
                "Policy: use_sync_output={} use_scroll_region={}",
                yes_no(caps.use_sync_output()),
                yes_no(caps.use_scroll_region())
            ),
            format!(
                "Policy: use_hyperlinks={} use_clipboard={}",
                yes_no(caps.use_hyperlinks()),
                yes_no(caps.use_clipboard())
            ),
            format!(
                "Passthrough wrap: {}",
                yes_no(caps.needs_passthrough_wrap())
            ),
        ];

        for (idx, line) in lines.iter().enumerate() {
            if idx as u16 >= inner.height {
                break;
            }
            let row_area = Rect::new(inner.x, inner.y + idx as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::MUTED))
                .render(row_area, frame);
        }
    }

    fn report_path() -> String {
        std::env::var("FTUI_TERMCAPS_REPORT_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "terminal_caps_report.jsonl".to_string())
    }

    fn export_report(
        &self,
        detected: &TerminalCapabilities,
        simulated: &TerminalCapabilities,
        active_rows: &[CapabilityRow],
        env: &EnvSnapshot,
        timestamp_ms: u64,
    ) -> ReportStatus {
        let path = Self::report_path();
        let comparison_rows = self.build_comparison_rows(detected, simulated);

        let rows = active_rows
            .iter()
            .zip(comparison_rows.iter())
            .map(|(active, comp)| {
                json!({
                    "capability": active.name,
                    "detected": comp.detected,
                    "simulated": comp.simulated,
                    "effective": active.effective,
                    "fallback": active.fallback.clone(),
                    "reason": active.reason.clone(),
                })
            })
            .collect::<Vec<_>>();

        let detected_profile = Self::profile_label(detected.profile());
        let simulated_profile =
            Self::profile_label(self.profile_override.unwrap_or(detected.profile()));

        let report = json!({
            "ts": timestamp_ms,
            "event": "terminal_caps_report",
            "detected_profile": detected_profile,
            "simulated_profile": simulated_profile,
            "simulation_active": self.profile_override.is_some(),
            "env": {
                "TERM": env.term.clone(),
                "TERM_PROGRAM": env.term_program.clone(),
                "COLORTERM": env.colorterm.clone(),
                "NO_COLOR": env.no_color,
                "TMUX": env.tmux,
                "SCREEN": env.screen,
                "ZELLIJ": env.zellij,
                "KITTY_WINDOW_ID": env.kitty,
                "WT_SESSION": env.wt_session,
            },
            "capabilities": rows,
        });

        let line = match serde_json::to_string(&report) {
            Ok(line) => line,
            Err(err) => {
                return ReportStatus {
                    path,
                    success: false,
                    error: Some(err.to_string()),
                };
            }
        };

        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| writeln!(file, "{}", line));

        match result {
            Ok(()) => ReportStatus {
                path,
                success: true,
                error: None,
            },
            Err(err) => ReportStatus {
                path,
                success: false,
                error: Some(err.to_string()),
            },
        }
    }
}

impl Screen for TerminalCapabilitiesScreen {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            let detected = self.detected_capabilities();
            let simulated = self.simulated_capabilities(&detected);
            let rows = self.build_rows(&simulated);
            let row_count = rows.len();
            let old_view = self.view;
            let old_selected = self.selected;
            let old_profile = self.profile_override;

            match code {
                KeyCode::Tab => {
                    self.view = self.view.next();
                    // Log view mode change
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged)
                        .with_view_mode(self.view)
                        .with_detail("from", old_view.label());
                    self.diagnostic_log.record(entry);

                    // Log additional events based on new view mode
                    match self.view {
                        ViewMode::Evidence => {
                            if let Some(row) = rows.get(self.selected) {
                                let entry = DiagnosticEntry::new(
                                    DiagnosticEventKind::EvidenceLedgerAccessed,
                                )
                                .with_capability(row.name);
                                self.diagnostic_log.record(entry);
                            }
                        }
                        ViewMode::Simulation => {
                            let profile_label = self
                                .profile_override
                                .map(Self::profile_label)
                                .unwrap_or("detected");
                            let entry =
                                DiagnosticEntry::new(DiagnosticEventKind::SimulationActivated)
                                    .with_profile(profile_label);
                            self.diagnostic_log.record(entry);
                        }
                        ViewMode::Matrix => {}
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_selection(-1, row_count);
                    if self.selected != old_selected {
                        let entry = DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged)
                            .with_selection(self.selected)
                            .with_detail("from", &old_selected.to_string());
                        self.diagnostic_log.record(entry);

                        // Log capability inspection
                        if let Some(row) = rows.get(self.selected) {
                            let entry =
                                DiagnosticEntry::new(DiagnosticEventKind::CapabilityInspected)
                                    .with_capability(row.name)
                                    .with_detail("detected", &row.detected.to_string())
                                    .with_detail("effective", &row.effective.to_string());
                            self.diagnostic_log.record(entry);
                        }
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_selection(1, row_count);
                    if self.selected != old_selected {
                        let entry = DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged)
                            .with_selection(self.selected)
                            .with_detail("from", &old_selected.to_string());
                        self.diagnostic_log.record(entry);

                        // Log capability inspection
                        if let Some(row) = rows.get(self.selected) {
                            let entry =
                                DiagnosticEntry::new(DiagnosticEventKind::CapabilityInspected)
                                    .with_capability(row.name)
                                    .with_detail("detected", &row.detected.to_string())
                                    .with_detail("effective", &row.effective.to_string());
                            self.diagnostic_log.record(entry);
                        }
                    }
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    self.cycle_profile();
                    let new_profile_label = self
                        .profile_override
                        .map(Self::profile_label)
                        .unwrap_or("detected");
                    let old_profile_label =
                        old_profile.map(Self::profile_label).unwrap_or("detected");
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::ProfileCycled)
                        .with_profile(new_profile_label)
                        .with_detail("from", old_profile_label);
                    self.diagnostic_log.record(entry);
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.reset_profile();
                    if old_profile.is_some() {
                        let old_profile_label =
                            old_profile.map(Self::profile_label).unwrap_or("detected");
                        let entry = DiagnosticEntry::new(DiagnosticEventKind::ProfileReset)
                            .with_detail("from", old_profile_label);
                        self.diagnostic_log.record(entry);
                    }
                }
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    let env = self.env_override.clone().unwrap_or_else(EnvSnapshot::read);
                    let mut entry = DiagnosticEntry::new(DiagnosticEventKind::ReportExported);
                    let report_status =
                        self.export_report(&detected, &simulated, &rows, &env, entry.timestamp_ms);
                    self.last_report = Some(report_status.clone());
                    entry = entry
                        .with_detail("path", &report_status.path)
                        .with_detail("status", if report_status.success { "ok" } else { "error" })
                        .with_profile(
                            self.profile_override
                                .map(Self::profile_label)
                                .unwrap_or("detected"),
                        );
                    self.diagnostic_log.record(entry);
                }
                // Quick profile selection: 1-5 pick common profiles
                // 1=Modern, 2=Xterm256, 3=Vt100, 4=Dumb, 5=Tmux
                KeyCode::Char(ch @ '1'..='5') => {
                    let idx = ((*ch) as usize) - ('0' as usize);
                    let old_profile_label =
                        old_profile.map(Self::profile_label).unwrap_or("detected");
                    self.set_profile_by_index(idx);
                    let new_profile_label = self
                        .profile_override
                        .map(Self::profile_label)
                        .unwrap_or("detected");
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::ProfileCycled)
                        .with_profile(new_profile_label)
                        .with_detail("from", old_profile_label)
                        .with_detail("via", &format!("key_{ch}"));
                    self.diagnostic_log.record(entry);
                }
                // 0 resets to detected profile
                KeyCode::Char('0') => {
                    if old_profile.is_some() {
                        let old_profile_label =
                            old_profile.map(Self::profile_label).unwrap_or("detected");
                        self.reset_profile();
                        let entry = DiagnosticEntry::new(DiagnosticEventKind::ProfileReset)
                            .with_detail("from", old_profile_label)
                            .with_detail("via", "key_0");
                        self.diagnostic_log.record(entry);
                    }
                }
                _ => {}
            }
        }

        // Mouse events
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse);
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let detected = self.detected_capabilities();
        let simulated = self.simulated_capabilities(&detected);
        let active = self.active_capabilities(&detected);
        let env = self.env_override.clone().unwrap_or_else(EnvSnapshot::read);
        let rows = self.build_rows(&active);
        let comparison_rows = self.build_comparison_rows(&detected, &simulated);

        let layout = Flex::vertical()
            .constraints([Constraint::Fixed(5), Constraint::Fill])
            .split(area);

        self.render_summary(frame, layout[0], &detected, &active);

        let body = layout[1];
        if body.is_empty() {
            return;
        }

        let columns = Flex::horizontal()
            .constraints([Constraint::Percentage(58.0), Constraint::Percentage(42.0)])
            .split(body);

        let left = columns[0];
        let right = columns[1];

        if self.view == ViewMode::Simulation && self.profile_override.is_some() {
            self.render_comparison_panel(frame, left, &comparison_rows);
        } else {
            self.render_matrix_panel(frame, left, &rows);
        }

        let right_chunks = Flex::vertical()
            .constraints([
                Constraint::Percentage(55.0),
                Constraint::Percentage(25.0),
                Constraint::Percentage(20.0),
            ])
            .split(right);

        if let Some(selected_row) = self.selected_row(&rows) {
            let comparison = if self.profile_override.is_some() {
                comparison_rows.get(self.selected)
            } else {
                None
            };
            match self.view {
                ViewMode::Matrix => {
                    self.render_details_panel(frame, right_chunks[0], selected_row, comparison)
                }
                ViewMode::Evidence => {
                    self.render_evidence_panel(frame, right_chunks[0], &active, selected_row)
                }
                ViewMode::Simulation => {
                    self.render_simulation_panel(frame, right_chunks[0], &active)
                }
            }
        }

        self.render_policy_panel(frame, right_chunks[1], &active);
        self.render_environment_panel(frame, right_chunks[2], &env, &active);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Tab",
                action: "Cycle view (matrix/evidence/simulation)",
            },
            HelpEntry {
                key: "↑/↓/j/k",
                action: "Select capability",
            },
            HelpEntry {
                key: "P",
                action: "Cycle simulated profile",
            },
            HelpEntry {
                key: "0-5",
                action: "Quick profile (0=detected, 1=modern..5=tmux)",
            },
            HelpEntry {
                key: "R",
                action: "Reset to detected profile",
            },
            HelpEntry {
                key: "E",
                action: "Export JSONL capability report",
            },
            HelpEntry {
                key: "Click",
                action: "Select capability row",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate capabilities",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Terminal Capabilities"
    }

    fn tab_label(&self) -> &'static str {
        "Caps"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn policy_reason(detected: bool, effective: bool, in_mux: bool) -> &'static str {
    if !detected {
        "terminal lacks support"
    } else if effective {
        "enabled"
    } else if in_mux {
        "disabled in mux"
    } else {
        "policy disabled"
    }
}

fn evidence_label(source: EvidenceSource) -> &'static str {
    match source {
        EvidenceSource::Environment => "environment",
        EvidenceSource::Da1Response => "da1",
        EvidenceSource::Da2Response => "da2",
        EvidenceSource::DecrpmResponse => "decrpm",
        EvidenceSource::OscResponse => "osc",
        EvidenceSource::Timeout => "timeout",
        EvidenceSource::Prior => "prior",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;
    use proptest::prelude::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn jsonl_timestamp() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("T{n:06}")
    }

    fn log_jsonl(step: &str, fields: &[(&str, String)]) {
        let mut parts = Vec::with_capacity(fields.len() + 2);
        parts.push(format!("\"ts\":\"{}\"", jsonl_timestamp()));
        parts.push(format!("\"step\":\"{}\"", step));
        parts.extend(fields.iter().map(|(k, v)| format!("\"{}\":\"{}\"", k, v)));
        eprintln!("{{{}}}", parts.join(","));
    }

    fn checksum_frame(frame: &Frame) -> u64 {
        let mut hasher = DefaultHasher::new();
        let width = frame.buffer.width();
        let height = frame.buffer.height();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = frame.buffer.get(x, y) {
                    cell.content.hash(&mut hasher);
                    cell.fg.hash(&mut hasher);
                    cell.bg.hash(&mut hasher);
                    cell.attrs.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }

    #[test]
    fn diagnostic_entry_jsonl_format() {
        reset_diagnostic_seq();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged)
            .with_view_mode(ViewMode::Evidence)
            .with_detail("from", "Matrix");

        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\"kind\":\"view_mode_changed\""));
        assert!(jsonl.contains("\"view_mode\":\"Evidence\""));
        assert!(jsonl.contains("\"from\":\"Matrix\""));
        assert!(jsonl.contains("\"seq\":"));
        assert!(jsonl.contains("\"ts\":"));
    }

    #[test]
    fn diagnostic_entry_escapes_special_chars() {
        reset_diagnostic_seq();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::CapabilityInspected)
            .with_detail("name", "test\"value\nwith\ttabs");

        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\\\""));
        assert!(jsonl.contains("\\n"));
        assert!(jsonl.contains("\\t"));
    }

    #[test]
    fn diagnostic_log_records_entries() {
        let mut log = DiagnosticLog::new();
        assert!(log.entries().is_empty());

        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ProfileCycled));

        assert_eq!(log.entries().len(), 3);
        assert_eq!(log.entries()[0].kind, DiagnosticEventKind::ViewModeChanged);
        assert_eq!(log.entries()[1].kind, DiagnosticEventKind::SelectionChanged);
        assert_eq!(log.entries()[2].kind, DiagnosticEventKind::ProfileCycled);
    }

    #[test]
    fn diagnostic_log_max_entries() {
        let mut log = DiagnosticLog::new().with_max_entries(3);

        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ProfileCycled));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ProfileReset));

        assert_eq!(log.entries().len(), 3);
        // First entry should have been removed
        assert_eq!(log.entries()[0].kind, DiagnosticEventKind::SelectionChanged);
    }

    #[test]
    fn diagnostic_log_clear() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));
        assert_eq!(log.entries().len(), 2);

        log.clear();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_summary_counts() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ProfileCycled));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ProfileReset));
        log.record(DiagnosticEntry::new(
            DiagnosticEventKind::CapabilityInspected,
        ));
        log.record(DiagnosticEntry::new(
            DiagnosticEventKind::EvidenceLedgerAccessed,
        ));
        log.record(DiagnosticEntry::new(
            DiagnosticEventKind::SimulationActivated,
        ));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::EnvironmentRead));

        let summary = log.summary();
        assert_eq!(summary.view_mode_changed_count, 2);
        assert_eq!(summary.selection_changed_count, 1);
        assert_eq!(summary.profile_cycled_count, 1);
        assert_eq!(summary.profile_reset_count, 1);
        assert_eq!(summary.capability_inspected_count, 1);
        assert_eq!(summary.evidence_ledger_accessed_count, 1);
        assert_eq!(summary.simulation_activated_count, 1);
        assert_eq!(summary.environment_read_count, 1);
        assert_eq!(summary.report_exported_count, 0);
        assert_eq!(summary.total(), 9);
    }

    #[test]
    fn diagnostic_summary_jsonl_format() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));

        let summary = log.summary();
        let jsonl = summary.to_jsonl();
        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("\"summary\":true"));
        assert!(jsonl.contains("\"total\":2"));
        assert!(jsonl.contains("\"view_mode_changed\":1"));
        assert!(jsonl.contains("\"selection_changed\":1"));
    }

    #[test]
    fn diagnostic_log_to_jsonl() {
        reset_diagnostic_seq();
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));

        let jsonl = log.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"kind\":\"view_mode_changed\""));
        assert!(lines[1].contains("\"kind\":\"selection_changed\""));
    }

    #[test]
    fn diagnostic_entries_of_kind() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SelectionChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ViewModeChanged));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ProfileCycled));

        let view_changes = log.entries_of_kind(DiagnosticEventKind::ViewModeChanged);
        assert_eq!(view_changes.len(), 2);

        let profile_changes = log.entries_of_kind(DiagnosticEventKind::ProfileCycled);
        assert_eq!(profile_changes.len(), 1);

        let resets = log.entries_of_kind(DiagnosticEventKind::ProfileReset);
        assert!(resets.is_empty());
    }

    #[test]
    fn screen_logs_view_mode_changes() {
        reset_diagnostic_seq();
        let mut screen = TerminalCapabilitiesScreen::new();

        // Simulate Tab key press
        let event = Event::Key(KeyEvent {
            code: KeyCode::Tab,
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        screen.update(&event);

        let entries = screen.diagnostic_log().entries();
        assert!(!entries.is_empty());

        let view_changes = screen
            .diagnostic_log()
            .entries_of_kind(DiagnosticEventKind::ViewModeChanged);
        assert_eq!(view_changes.len(), 1);
        assert_eq!(screen.view, ViewMode::Evidence);
    }

    #[test]
    fn screen_logs_selection_changes() {
        reset_diagnostic_seq();
        let mut screen = TerminalCapabilitiesScreen::new();

        // Simulate Down key press
        let event = Event::Key(KeyEvent {
            code: KeyCode::Down,
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        screen.update(&event);

        let selection_changes = screen
            .diagnostic_log()
            .entries_of_kind(DiagnosticEventKind::SelectionChanged);
        assert_eq!(selection_changes.len(), 1);

        let capability_inspections = screen
            .diagnostic_log()
            .entries_of_kind(DiagnosticEventKind::CapabilityInspected);
        assert_eq!(capability_inspections.len(), 1);
    }

    #[test]
    fn screen_logs_profile_cycle() {
        reset_diagnostic_seq();
        let mut screen = TerminalCapabilitiesScreen::new();

        // Simulate P key press
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        screen.update(&event);

        let profile_cycles = screen
            .diagnostic_log()
            .entries_of_kind(DiagnosticEventKind::ProfileCycled);
        assert_eq!(profile_cycles.len(), 1);
    }

    #[test]
    fn render_modes_are_deterministic_with_env_override() {
        // Acquire combined render lock BEFORE creating the screen to prevent race with
        // parallel tests that mutate global theme/accessibility state.
        let _render_guard =
            theme::ScopedRenderLock::new(theme::ThemeId::CyberpunkAurora, false, 1.0);

        let mut screen = TerminalCapabilitiesScreen::with_profile(TerminalProfile::Modern);
        screen.set_env_override(EnvSnapshot::from_values(
            "xterm-256color",
            "ftui-test",
            "truecolor",
            false,
            false,
            false,
            false,
            false,
            false,
        ));

        for view in [ViewMode::Matrix, ViewMode::Evidence, ViewMode::Simulation] {
            screen.view = view;
            let area = Rect::new(0, 0, 120, 40);

            let checksum_first = {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                screen.view(&mut frame, area);
                checksum_frame(&frame)
            };

            let checksum_second = {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                screen.view(&mut frame, area);
                checksum_frame(&frame)
            };

            log_jsonl(
                "render",
                &[
                    ("case", "terminal_caps_render_deterministic".to_string()),
                    ("view", view.label().to_string()),
                    ("checksum", format!("{checksum_first:016x}")),
                ],
            );

            assert_eq!(
                checksum_first,
                checksum_second,
                "render should be deterministic for view {}",
                view.label()
            );
            assert!(checksum_first != 0, "checksum should be non-zero");
        }
    }

    #[test]
    fn profile_cycle_wraps_and_reset_clears_override() {
        let mut screen = TerminalCapabilitiesScreen::new();
        let order = TerminalCapabilitiesScreen::profile_order();
        assert!(!order.is_empty());

        for _ in 0..order.len() {
            screen.cycle_profile();
        }

        // After cycling through all profiles, override should be None (Detected).
        assert!(screen.profile_override.is_none());

        screen.cycle_profile();
        assert!(screen.profile_override.is_some());

        screen.reset_profile();
        assert!(screen.profile_override.is_none());

        log_jsonl(
            "profile_cycle",
            &[
                ("case", "terminal_caps_profile_cycle".to_string()),
                ("profiles", order.len().to_string()),
            ],
        );
    }

    #[test]
    fn regression_build_rows_names_stable() {
        let screen = TerminalCapabilitiesScreen::new();
        let rows = screen.build_rows(&TerminalCapabilities::from_profile(TerminalProfile::Modern));

        let names: Vec<&'static str> = rows.iter().map(|row| row.name).collect();
        let expected = vec![
            "True color (24-bit)",
            "256-color palette",
            "Synchronized output",
            "Scroll region (DECSTBM)",
            "OSC 8 hyperlinks",
            "Kitty keyboard protocol",
            "Focus events",
            "Bracketed paste",
            "SGR mouse",
            "OSC 52 clipboard",
        ];
        assert_eq!(names, expected);

        log_jsonl(
            "regression",
            &[
                ("case", "terminal_caps_rows_stable".to_string()),
                ("rows", names.len().to_string()),
            ],
        );
    }

    #[test]
    fn regression_reason_for_mux_policy() {
        let caps = TerminalCapabilities::from_profile(TerminalProfile::Tmux);
        let mux_reason = TerminalCapabilitiesScreen::reason_for(&caps, true, false, true);
        assert_eq!(mux_reason, "Disabled by mux safety policy");

        let non_mux = TerminalCapabilities::from_profile(TerminalProfile::Modern);
        let non_mux_reason = TerminalCapabilitiesScreen::reason_for(&non_mux, true, false, true);
        assert_eq!(non_mux_reason, "Policy disabled (safety)");

        log_jsonl(
            "regression",
            &[
                ("case", "terminal_caps_reason_for".to_string()),
                ("mux", mux_reason),
                ("non_mux", non_mux_reason),
            ],
        );
    }

    #[test]
    fn regression_fallback_text() {
        assert_eq!(
            TerminalCapabilitiesScreen::fallback_text(true, "fallback"),
            "enabled"
        );
        assert_eq!(
            TerminalCapabilitiesScreen::fallback_text(false, "fallback"),
            "fallback"
        );
    }

    #[test]
    fn regression_profile_label_detected() {
        assert_eq!(
            TerminalCapabilitiesScreen::profile_label(TerminalProfile::Detected),
            "detected"
        );
        assert_eq!(
            TerminalCapabilitiesScreen::profile_label(TerminalProfile::Xterm),
            TerminalProfile::Xterm.as_str()
        );
    }

    proptest! {
        #[test]
        fn prop_selection_wraps_within_bounds(row_count in 1usize..30, start in 0usize..200) {
            let mut screen = TerminalCapabilitiesScreen::new();
            let start = start % row_count;

            screen.selected = start;
            screen.move_selection(1, row_count);
            prop_assert!(screen.selected < row_count);
            if start + 1 == row_count {
                prop_assert_eq!(screen.selected, 0);
            }

            screen.selected = start;
            screen.move_selection(-1, row_count);
            prop_assert!(screen.selected < row_count);
            if start == 0 {
                prop_assert_eq!(screen.selected, row_count - 1);
            }
        }
    }

    #[test]
    fn event_kind_as_str() {
        assert_eq!(
            DiagnosticEventKind::ViewModeChanged.as_str(),
            "view_mode_changed"
        );
        assert_eq!(
            DiagnosticEventKind::SelectionChanged.as_str(),
            "selection_changed"
        );
        assert_eq!(
            DiagnosticEventKind::ProfileCycled.as_str(),
            "profile_cycled"
        );
        assert_eq!(DiagnosticEventKind::ProfileReset.as_str(), "profile_reset");
        assert_eq!(
            DiagnosticEventKind::CapabilityInspected.as_str(),
            "capability_inspected"
        );
        assert_eq!(
            DiagnosticEventKind::EvidenceLedgerAccessed.as_str(),
            "evidence_ledger_accessed"
        );
        assert_eq!(
            DiagnosticEventKind::SimulationActivated.as_str(),
            "simulation_activated"
        );
        assert_eq!(
            DiagnosticEventKind::EnvironmentRead.as_str(),
            "environment_read"
        );
        assert_eq!(
            DiagnosticEventKind::ReportExported.as_str(),
            "report_exported"
        );
    }

    #[test]
    fn policy_reason_explains_mux_and_support() {
        assert_eq!(policy_reason(false, false, false), "terminal lacks support");
        assert_eq!(policy_reason(true, true, false), "enabled");
        assert_eq!(policy_reason(true, false, true), "disabled in mux");
        assert_eq!(policy_reason(true, false, false), "policy disabled");
    }

    // -----------------------------------------------------------------------
    // Mouse + keyboard shortcut tests (bd-iuvb.17.13.1)
    // -----------------------------------------------------------------------

    #[test]
    fn number_keys_select_profiles() {
        let mut screen = TerminalCapabilitiesScreen::new();
        assert!(screen.profile_override.is_none());

        // Key '1' → Modern
        let key1 = Event::Key(KeyEvent {
            code: KeyCode::Char('1'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&key1);
        assert_eq!(screen.profile_override, Some(TerminalProfile::Modern));

        // Key '0' → reset to detected
        let key0 = Event::Key(KeyEvent {
            code: KeyCode::Char('0'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&key0);
        assert!(screen.profile_override.is_none());
    }

    #[test]
    fn number_key_5_selects_tmux() {
        let mut screen = TerminalCapabilitiesScreen::new();
        let key5 = Event::Key(KeyEvent {
            code: KeyCode::Char('5'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&key5);
        // Profile order index 5 = Dumb (0=Detected,1=Modern,2=Xterm256,3=Xterm,4=Vt100,5=Dumb)
        assert_eq!(screen.profile_override, Some(TerminalProfile::Dumb));
    }

    #[test]
    fn mouse_click_selects_row() {
        let mut screen = TerminalCapabilitiesScreen::new();
        assert_eq!(screen.selected, 0);

        // Simulate having rendered the matrix panel at a known area.
        screen.last_matrix_area.set(Rect::new(0, 0, 80, 20));

        // Click on row 3 (y = 0 border + 1 header + 3 = row index 3).
        let click = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            10,
            5, // border(1) + header(1) + row_idx(3) = y=5
        ));
        screen.update(&click);
        assert_eq!(screen.selected, 3, "Click should select row 3");
    }

    #[test]
    fn mouse_scroll_navigates() {
        let mut screen = TerminalCapabilitiesScreen::new();
        assert_eq!(screen.selected, 0);

        screen.last_matrix_area.set(Rect::new(0, 0, 80, 20));

        let scroll_down = Event::Mouse(MouseEvent::new(MouseEventKind::ScrollDown, 10, 5));
        screen.update(&scroll_down);
        assert_eq!(screen.selected, 1, "Scroll down should move selection down");

        let scroll_up = Event::Mouse(MouseEvent::new(MouseEventKind::ScrollUp, 10, 5));
        screen.update(&scroll_up);
        assert_eq!(screen.selected, 0, "Scroll up should move selection back");
    }

    #[test]
    fn keybindings_has_at_least_three() {
        let screen = TerminalCapabilitiesScreen::new();
        let bindings = screen.keybindings();
        assert!(
            bindings.len() >= 3,
            "Expected at least 3 keybindings, got {}",
            bindings.len()
        );
    }
}
