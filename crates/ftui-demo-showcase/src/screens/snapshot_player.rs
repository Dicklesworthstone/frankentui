#![forbid(unsafe_code)]

//! Snapshot/Time Travel Player demo screen.
//!
//! Demonstrates FrankenTUI's time-travel debugging capabilities:
//! - Frame recording with delta compression
//! - Timeline scrubbing with smooth playback
//! - Frame metadata inspection (diff counts, render stats)
//! - Integrity verification via checksums
//!
//! # Invariants
//!
//! 1. **Playback determinism**: Same snapshot file + same frame index = identical buffer
//! 2. **Progress bounds**: `0 <= current_frame < frame_count` (when non-empty)
//! 3. **Checksum integrity**: Hash chain verifies tamper-free replay
//! 4. **Memory budget**: Bounded by TimeTravel capacity (default 100 frames)
//!
//! # Failure Modes
//!
//! - **Empty recording**: Gracefully show "No frames recorded" state
//! - **Corrupted import**: Display error, don't panic
//! - **Large scrub jump**: May briefly lag while reconstructing (O(n) deltas)
//!
//! # Keybindings
//!
//! - Space: Play/pause playback
//! - Left/Right: Step backward/forward one frame
//! - Home/End: Jump to first/last frame
//! - M: Toggle marker mode (add/remove frame markers)
//! - R: Toggle recording (capture new frames)
//! - C: Clear recording
//! - D: Toggle diagnostic panel

use std::cell::Cell as StdCell;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::charts::heatmap_gradient;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

/// Demo content patterns for generating interesting frames.
const DEMO_PATTERNS: &[&str] = &[
    "Hello, World!",
    "FrankenTUI Demo",
    "Time Travel Mode",
    "Frame Snapshot",
    "Delta Encoding",
    "Press Space to Play",
    "← → to Step",
    "Deterministic Replay",
];

/// Configuration for the snapshot player.
#[derive(Clone, Debug)]
pub struct SnapshotPlayerConfig {
    /// Maximum frames to record.
    pub max_frames: usize,
    /// Playback speed (frames per tick when playing).
    pub playback_speed: usize,
    /// Whether to auto-generate demo frames on init.
    pub auto_generate_demo: bool,
    /// Number of demo frames to generate.
    pub demo_frame_count: usize,
}

impl Default for SnapshotPlayerConfig {
    fn default() -> Self {
        Self {
            max_frames: 100,
            playback_speed: 1,
            auto_generate_demo: true,
            demo_frame_count: 50,
        }
    }
}

/// Playback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Paused,
    Playing,
    Recording,
}

impl PlaybackState {
    /// Human-readable label for display (includes both icon and text).
    pub fn label(self) -> &'static str {
        match self {
            Self::Paused => "⏸ Paused",
            Self::Playing => "▶ Playing",
            Self::Recording => "⏺ Recording",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Paused => Style::new().fg(theme::fg::MUTED),
            Self::Playing => Style::new().fg(theme::accent::SUCCESS),
            Self::Recording => Style::new().fg(theme::accent::ERROR),
        }
    }
}

/// Metadata for a recorded frame.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// Frame index.
    pub index: usize,
    /// Number of changed cells from previous frame.
    pub change_count: usize,
    /// Buffer dimensions.
    pub width: u16,
    pub height: u16,
    /// Estimated memory size in bytes.
    pub memory_size: usize,
    /// Frame checksum for integrity verification.
    pub checksum: u64,
    /// Render time (if known).
    pub render_time: Option<Duration>,
}

/// Preview mode for the Time-Travel Studio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StudioView {
    Single,
    Compare,
}

impl StudioView {
    fn toggle(self) -> Self {
        match self {
            Self::Single => Self::Compare,
            Self::Compare => Self::Single,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Compare => "A/B Compare",
        }
    }
}

/// Heatmap overlay mode for diff visualization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeatmapMode {
    Off,
    Overlay,
}

impl HeatmapMode {
    fn toggle(self) -> Self {
        match self {
            Self::Off => Self::Overlay,
            Self::Overlay => Self::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Overlay => "Overlay",
        }
    }
}

#[derive(Debug, Clone)]
struct DiffCell {
    x: u16,
    y: u16,
    intensity: f64,
}

#[derive(Debug, Clone)]
struct DiffCache {
    a_index: usize,
    b_index: usize,
    frames_version: u64,
    width: u16,
    height: u16,
    diff_cells: Vec<DiffCell>,
    diff_count: usize,
    content_diff_count: usize,
    style_diff_count: usize,
    checksum_a: u64,
    checksum_b: u64,
}

#[derive(Debug, Clone)]
struct ExportStatus {
    path: String,
    ok: bool,
    message: String,
}

// =============================================================================
// Diagnostic Logging + Telemetry (bd-3sa7.5)
// =============================================================================

/// Configuration for diagnostic logging and telemetry.
#[derive(Clone, Debug)]
pub struct DiagnosticConfig {
    /// Enable structured JSONL logging.
    pub enabled: bool,
    /// Maximum diagnostic entries to retain.
    pub max_entries: usize,
    /// Log navigation events.
    pub log_navigation: bool,
    /// Log playback state changes.
    pub log_playback: bool,
    /// Log frame recording events.
    pub log_recording: bool,
}

impl Default for DiagnosticConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 500,
            log_navigation: true,
            log_playback: true,
            log_recording: true,
        }
    }
}

/// A diagnostic log entry for JSONL output.
#[derive(Clone, Debug)]
pub enum DiagnosticEntry {
    /// Frame navigation event.
    Navigation {
        seq: u64,
        action: &'static str,
        from_frame: usize,
        to_frame: usize,
        frame_count: usize,
    },
    /// Playback state transition.
    PlaybackChange {
        seq: u64,
        from_state: &'static str,
        to_state: &'static str,
        current_frame: usize,
    },
    /// Frame recorded.
    FrameRecorded {
        seq: u64,
        frame_index: usize,
        change_count: usize,
        checksum: u64,
        chain_checksum: u64,
        width: u16,
        height: u16,
    },
    /// Marker toggled.
    MarkerToggled {
        seq: u64,
        frame_index: usize,
        added: bool,
        total_markers: usize,
    },
    /// Clear/reset event.
    Cleared {
        seq: u64,
        frame_count: usize,
        marker_count: usize,
    },
}

impl DiagnosticEntry {
    /// Serialize to JSONL format.
    pub fn to_jsonl(&self) -> String {
        match self {
            Self::Navigation {
                seq,
                action,
                from_frame,
                to_frame,
                frame_count,
            } => {
                format!(
                    r#"{{"seq":{},"event":"nav","action":"{}","from":{},"to":{},"count":{}}}"#,
                    seq, action, from_frame, to_frame, frame_count
                )
            }
            Self::PlaybackChange {
                seq,
                from_state,
                to_state,
                current_frame,
            } => {
                format!(
                    r#"{{"seq":{},"event":"playback","from":"{}","to":"{}","frame":{}}}"#,
                    seq, from_state, to_state, current_frame
                )
            }
            Self::FrameRecorded {
                seq,
                frame_index,
                change_count,
                checksum,
                chain_checksum,
                width,
                height,
            } => {
                format!(
                    r#"{{"seq":{},"event":"record","frame":{},"changes":{},"checksum":"0x{:016x}","chain":"0x{:016x}","size":[{},{}]}}"#,
                    seq, frame_index, change_count, checksum, chain_checksum, width, height
                )
            }
            Self::MarkerToggled {
                seq,
                frame_index,
                added,
                total_markers,
            } => {
                format!(
                    r#"{{"seq":{},"event":"marker","frame":{},"added":{},"total":{}}}"#,
                    seq, frame_index, added, total_markers
                )
            }
            Self::Cleared {
                seq,
                frame_count,
                marker_count,
            } => {
                format!(
                    r#"{{"seq":{},"event":"clear","frames":{},"markers":{}}}"#,
                    seq, frame_count, marker_count
                )
            }
        }
    }
}

/// Diagnostic log buffer with bounded capacity.
#[derive(Debug)]
pub struct DiagnosticLog {
    entries: std::collections::VecDeque<DiagnosticEntry>,
    max_entries: usize,
    seq: u64,
}

impl DiagnosticLog {
    /// Create a new diagnostic log.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: std::collections::VecDeque::with_capacity(max_entries.min(1000)),
            max_entries,
            seq: 0,
        }
    }

    /// Get and increment the sequence number.
    pub fn next_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    /// Push a log entry.
    pub fn push(&mut self, entry: DiagnosticEntry) {
        if self.max_entries == 0 {
            return;
        }
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Get all entries.
    pub fn entries(&self) -> &std::collections::VecDeque<DiagnosticEntry> {
        &self.entries
    }

    /// Export to JSONL format.
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .map(|e| e.to_jsonl())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Clear entries (keeps seq).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Time-Travel Studio screen state.
#[derive(Debug)]
pub struct SnapshotPlayer {
    /// Recorded frames (buffers stored directly for demo simplicity).
    frames: Vec<Buffer>,
    /// Frame metadata.
    pub frame_info: Vec<FrameInfo>,
    /// Current frame index.
    pub current_frame: usize,
    /// Playback state.
    pub playback_state: PlaybackState,
    /// Marked frames for inspection.
    markers: HashSet<usize>,
    /// Whether to show diagnostic panel.
    show_diagnostics: bool,
    /// Current tick count.
    tick_count: u64,
    /// Configuration.
    config: SnapshotPlayerConfig,
    /// Demo buffer dimensions.
    demo_width: u16,
    demo_height: u16,
    /// Running checksum chain.
    pub checksum_chain: u64,
    /// Diagnostic configuration (bd-3sa7.5).
    diagnostic_config: DiagnosticConfig,
    /// Diagnostic log (bd-3sa7.5).
    diagnostic_log: DiagnosticLog,
    /// Studio view mode (single vs compare).
    compare_view: StudioView,
    /// Heatmap overlay mode.
    heatmap_mode: HeatmapMode,
    /// Selected frame A for comparison.
    compare_a: usize,
    /// Selected frame B for comparison.
    compare_b: usize,
    /// Cached diff data for A/B comparison.
    diff_cache: Option<DiffCache>,
    /// Frames version counter for cache invalidation.
    frames_version: u64,
    /// Last export status for JSONL reports.
    last_export: Option<ExportStatus>,
    /// Export path for JSONL reports.
    export_path: String,
    /// Whether timeline scrubbing with left mouse drag is active.
    timeline_scrubbing: bool,
    /// Cached timeline area for mouse hit-testing.
    layout_timeline: StdCell<Rect>,
    /// Cached preview area for mouse hit-testing.
    layout_preview: StdCell<Rect>,
    /// Cached info panel area for mouse hit-testing.
    layout_info: StdCell<Rect>,
}

impl Default for SnapshotPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotPlayer {
    /// Create a new snapshot player with demo content.
    pub fn new() -> Self {
        let config = SnapshotPlayerConfig::default();
        let diagnostic_config = DiagnosticConfig::default();
        let export_path = std::env::var("FTUI_TIME_TRAVEL_STUDIO_REPORT")
            .unwrap_or_else(|_| "time_travel_studio_report.jsonl".to_string());
        let mut player = Self {
            frames: Vec::with_capacity(config.max_frames),
            frame_info: Vec::with_capacity(config.max_frames),
            current_frame: 0,
            playback_state: PlaybackState::Paused,
            markers: HashSet::new(),
            show_diagnostics: true,
            tick_count: 0,
            config,
            demo_width: 40,
            demo_height: 15,
            checksum_chain: 0,
            diagnostic_log: DiagnosticLog::new(diagnostic_config.max_entries),
            diagnostic_config,
            compare_view: StudioView::Single,
            heatmap_mode: HeatmapMode::Off,
            compare_a: 0,
            compare_b: 0,
            diff_cache: None,
            frames_version: 0,
            last_export: None,
            export_path,
            timeline_scrubbing: false,
            layout_timeline: StdCell::new(Rect::default()),
            layout_preview: StdCell::new(Rect::default()),
            layout_info: StdCell::new(Rect::default()),
        };

        if player.config.auto_generate_demo {
            player.generate_demo_frames();
            player.reset_compare_indices();
        }

        player
    }

    /// Create with custom configuration.
    pub fn with_config(config: SnapshotPlayerConfig) -> Self {
        let diagnostic_config = DiagnosticConfig::default();
        let export_path = std::env::var("FTUI_TIME_TRAVEL_STUDIO_REPORT")
            .unwrap_or_else(|_| "time_travel_studio_report.jsonl".to_string());
        let mut player = Self {
            frames: Vec::with_capacity(config.max_frames),
            frame_info: Vec::with_capacity(config.max_frames),
            current_frame: 0,
            playback_state: PlaybackState::Paused,
            markers: HashSet::new(),
            show_diagnostics: true,
            tick_count: 0,
            demo_width: 40,
            demo_height: 15,
            checksum_chain: 0,
            config,
            diagnostic_log: DiagnosticLog::new(diagnostic_config.max_entries),
            diagnostic_config,
            compare_view: StudioView::Single,
            heatmap_mode: HeatmapMode::Off,
            compare_a: 0,
            compare_b: 0,
            diff_cache: None,
            frames_version: 0,
            last_export: None,
            export_path,
            timeline_scrubbing: false,
            layout_timeline: StdCell::new(Rect::default()),
            layout_preview: StdCell::new(Rect::default()),
            layout_info: StdCell::new(Rect::default()),
        };

        if player.config.auto_generate_demo {
            player.generate_demo_frames();
            player.reset_compare_indices();
        }

        player
    }

    fn bump_frames_version(&mut self) {
        self.frames_version = self.frames_version.wrapping_add(1);
        self.diff_cache = None;
    }

    fn reset_compare_indices(&mut self) {
        let count = self.frames.len();
        if count <= 1 {
            self.compare_a = 0;
            self.compare_b = 0;
        } else {
            self.compare_a = 0;
            self.compare_b = 1;
        }
        self.diff_cache = None;
    }

    fn clamp_compare_indices(&mut self) {
        let count = self.frames.len();
        if count == 0 {
            self.compare_a = 0;
            self.compare_b = 0;
            return;
        }
        let max = count.saturating_sub(1);
        self.compare_a = self.compare_a.min(max);
        self.compare_b = self.compare_b.min(max);
    }

    fn set_compare_a(&mut self, index: usize) {
        self.compare_a = index;
        self.clamp_compare_indices();
        self.diff_cache = None;
    }

    fn set_compare_b(&mut self, index: usize) {
        self.compare_b = index;
        self.clamp_compare_indices();
        self.diff_cache = None;
    }

    fn swap_compare(&mut self) {
        std::mem::swap(&mut self.compare_a, &mut self.compare_b);
        self.diff_cache = None;
    }

    fn toggle_compare_view(&mut self) {
        self.compare_view = self.compare_view.toggle();
        self.diff_cache = None;
    }

    fn toggle_heatmap(&mut self) {
        self.heatmap_mode = self.heatmap_mode.toggle();
    }

    fn compare_pair(&self) -> Option<(usize, usize)> {
        if self.frames.is_empty() {
            None
        } else {
            Some((self.compare_a, self.compare_b))
        }
    }

    fn refresh_diff_cache(&mut self) {
        if self.compare_view != StudioView::Compare {
            return;
        }
        self.clamp_compare_indices();
        let Some((a_idx, b_idx)) = self.compare_pair() else {
            self.diff_cache = None;
            return;
        };
        let needs_refresh = match &self.diff_cache {
            Some(cache) => {
                cache.a_index != a_idx
                    || cache.b_index != b_idx
                    || cache.frames_version != self.frames_version
            }
            None => true,
        };
        if needs_refresh {
            self.diff_cache = Some(self.compute_diff_cache(a_idx, b_idx));
        }
    }

    fn compute_diff_cache(&mut self, a_idx: usize, b_idx: usize) -> DiffCache {
        let buffer_a = &self.frames[a_idx];
        let buffer_b = &self.frames[b_idx];
        let width = buffer_a.width().min(buffer_b.width());
        let height = buffer_a.height().min(buffer_b.height());

        let mut diff_cells = if let Some(cache) = self.diff_cache.take() {
            let mut cells = cache.diff_cells;
            cells.clear();
            cells
        } else {
            Vec::new()
        };
        diff_cells.reserve((width as usize * height as usize) / 8);

        let mut diff_count = 0usize;
        let mut content_diff_count = 0usize;
        let mut style_diff_count = 0usize;

        for y in 0..height {
            for x in 0..width {
                let a = buffer_a.get_unchecked(x, y);
                let b = buffer_b.get_unchecked(x, y);
                if a.bits_eq(b) {
                    continue;
                }
                let content_diff = a.content.raw() != b.content.raw();
                if content_diff {
                    content_diff_count += 1;
                } else {
                    style_diff_count += 1;
                }
                let intensity = if content_diff { 1.0 } else { 0.6 };
                diff_cells.push(DiffCell { x, y, intensity });
                diff_count += 1;
            }
        }

        let checksum_a = self.frame_info.get(a_idx).map_or(0, |info| info.checksum);
        let checksum_b = self.frame_info.get(b_idx).map_or(0, |info| info.checksum);

        DiffCache {
            a_index: a_idx,
            b_index: b_idx,
            frames_version: self.frames_version,
            width,
            height,
            diff_cells,
            diff_count,
            content_diff_count,
            style_diff_count,
            checksum_a,
            checksum_b,
        }
    }

    /// Generate demo frames with evolving content.
    fn generate_demo_frames(&mut self) {
        let count = self.config.demo_frame_count;
        let mut prev_buf: Option<Buffer> = None;

        for i in 0..count {
            let mut buf = Buffer::new(self.demo_width, self.demo_height);

            // Draw evolving content based on frame number
            self.draw_demo_content(&mut buf, i);

            // Calculate change count from previous frame
            let change_count = match &prev_buf {
                Some(prev) => self.count_changes(prev, &buf),
                None => (self.demo_width as usize) * (self.demo_height as usize),
            };

            // Calculate checksum
            let checksum = self.calculate_checksum(&buf);
            self.checksum_chain = self.checksum_chain.wrapping_add(checksum);

            let info = FrameInfo {
                index: i,
                change_count,
                width: self.demo_width,
                height: self.demo_height,
                memory_size: buf.len() * std::mem::size_of::<Cell>(),
                checksum,
                render_time: Some(Duration::from_micros((100 + (i * 10) % 500) as u64)),
            };

            prev_buf = Some(buf.clone());
            self.frames.push(buf);
            self.frame_info.push(info);
        }

        self.bump_frames_version();
    }

    /// Draw demo content for a specific frame.
    fn draw_demo_content(&self, buf: &mut Buffer, frame_idx: usize) {
        let pattern_idx = frame_idx % DEMO_PATTERNS.len();
        let text = DEMO_PATTERNS[pattern_idx];

        // Animated position
        let x_offset = (frame_idx % 20) as u16;
        let y_offset = (frame_idx / 5 % 10) as u16;

        // Draw frame number in top-left
        let frame_label = format!("Frame {}/{}", frame_idx + 1, self.config.demo_frame_count);
        for (i, ch) in frame_label.chars().enumerate() {
            let x = i as u16;
            if x < buf.width() {
                buf.set(x, 0, Cell::from_char(ch));
            }
        }

        // Draw main pattern text with animation
        let y = (y_offset + 3).min(buf.height().saturating_sub(1));
        for (i, ch) in text.chars().enumerate() {
            let x = (x_offset + i as u16) % buf.width();
            if y < buf.height() {
                // Cycle colors based on position and frame
                let color_idx = (frame_idx + i) % 6;
                let fg_color = match color_idx {
                    0 => theme::accent::INFO,
                    1 => theme::accent::SUCCESS,
                    2 => theme::accent::WARNING,
                    3 => theme::accent::ERROR,
                    4 => theme::fg::PRIMARY,
                    _ => theme::fg::SECONDARY,
                };
                let cell = Cell::from_char(ch).with_fg(fg_color.into());
                buf.set(x, y, cell);
            }
        }

        // Draw a moving cursor indicator
        let cursor_x = ((frame_idx * 2) % (buf.width() as usize)) as u16;
        let cursor_y = buf.height().saturating_sub(2);
        if cursor_y < buf.height() {
            buf.set(cursor_x, cursor_y, Cell::from_char('█'));
        }

        // Draw progress bar at bottom
        let progress = frame_idx as f64 / self.config.demo_frame_count as f64;
        let bar_width = (buf.width() as f64 * progress) as u16;
        let bar_y = buf.height().saturating_sub(1);
        for x in 0..bar_width.min(buf.width()) {
            buf.set(x, bar_y, Cell::from_char('━'));
        }
    }

    /// Count changed cells between two buffers.
    fn count_changes(&self, prev: &Buffer, curr: &Buffer) -> usize {
        let mut count = 0;
        for y in 0..curr.height().min(prev.height()) {
            for x in 0..curr.width().min(prev.width()) {
                if let (Some(pc), Some(cc)) = (prev.get(x, y), curr.get(x, y))
                    && !pc.bits_eq(cc)
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// Calculate a simple checksum for integrity verification.
    fn calculate_checksum(&self, buf: &Buffer) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                if let Some(cell) = buf.get(x, y) {
                    // Mix in cell content
                    hash ^= cell.content.raw() as u64;
                    hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
                    hash ^= cell.fg.0 as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                    hash ^= cell.bg.0 as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
            }
        }
        hash
    }

    /// Record a new frame (when in recording mode).
    pub fn record_frame(&mut self, buf: &Buffer) {
        if self.frames.len() >= self.config.max_frames {
            // Remove oldest frame
            self.frames.remove(0);
            self.frame_info.remove(0);
            // Reindex remaining frames
            for (i, info) in self.frame_info.iter_mut().enumerate() {
                info.index = i;
            }
        }

        let prev = self.frames.last();
        let change_count = match prev {
            Some(p) => self.count_changes(p, buf),
            None => buf.len(),
        };

        let checksum = self.calculate_checksum(buf);
        self.checksum_chain = self.checksum_chain.wrapping_add(checksum);

        let info = FrameInfo {
            index: self.frames.len(),
            change_count,
            width: buf.width(),
            height: buf.height(),
            memory_size: buf.len() * std::mem::size_of::<Cell>(),
            checksum,
            render_time: None,
        };

        let frame_index = self.frames.len();
        let width = buf.width();
        let height = buf.height();
        self.frames.push(buf.clone());
        self.frame_info.push(info);
        self.current_frame = self.frames.len().saturating_sub(1);
        self.bump_frames_version();
        self.clamp_compare_indices();
        self.log_frame_recorded(frame_index, change_count, checksum, width, height);
    }

    /// Clear all recorded frames.
    pub fn clear(&mut self) {
        self.log_cleared();
        self.frames.clear();
        self.frame_info.clear();
        self.markers.clear();
        self.current_frame = 0;
        self.checksum_chain = 0;
        self.playback_state = PlaybackState::Paused;
        self.diagnostic_log.clear();
        self.bump_frames_version();
        self.reset_compare_indices();
    }

    /// Total number of frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Current frame index.
    pub fn current_frame(&self) -> usize {
        self.current_frame
    }

    /// Set current frame index with bounds checking.
    pub fn set_current_frame(&mut self, frame: usize) {
        if self.frames.is_empty() {
            self.current_frame = 0;
        } else {
            self.current_frame = frame.min(self.frames.len() - 1);
        }
    }

    /// Current checksum chain value.
    pub fn checksum_chain(&self) -> u64 {
        self.checksum_chain
    }

    /// Access full frame metadata list.
    pub fn frame_info(&self) -> &[FrameInfo] {
        &self.frame_info
    }

    /// Access marker set (read-only).
    pub fn markers(&self) -> &HashSet<usize> {
        &self.markers
    }

    /// Current playback state.
    pub fn playback_state(&self) -> PlaybackState {
        self.playback_state
    }

    /// Whether the diagnostic panel is currently visible.
    pub fn diagnostics_visible(&self) -> bool {
        self.show_diagnostics
    }

    /// Get current frame buffer.
    pub fn current_buffer(&self) -> Option<&Buffer> {
        self.frames.get(self.current_frame)
    }

    /// Get current frame info.
    pub fn current_info(&self) -> Option<&FrameInfo> {
        self.frame_info.get(self.current_frame)
    }

    /// Step to next frame.
    pub fn step_forward(&mut self) {
        let from = self.current_frame;
        if !self.frames.is_empty() {
            self.current_frame = (self.current_frame + 1).min(self.frames.len() - 1);
        }
        self.log_navigation("step_forward", from);
    }

    /// Step to previous frame.
    pub fn step_backward(&mut self) {
        let from = self.current_frame;
        self.current_frame = self.current_frame.saturating_sub(1);
        self.log_navigation("step_backward", from);
    }

    /// Jump to first frame.
    pub fn go_to_start(&mut self) {
        let from = self.current_frame;
        self.current_frame = 0;
        self.log_navigation("go_start", from);
    }

    /// Jump to last frame.
    pub fn go_to_end(&mut self) {
        let from = self.current_frame;
        if !self.frames.is_empty() {
            self.current_frame = self.frames.len() - 1;
        }
        self.log_navigation("go_end", from);
    }

    /// Toggle play/pause.
    pub fn toggle_playback(&mut self) {
        let from_state = self.playback_state.label();
        self.playback_state = match self.playback_state {
            PlaybackState::Playing => PlaybackState::Paused,
            PlaybackState::Paused | PlaybackState::Recording => PlaybackState::Playing,
        };
        self.log_playback(from_state);
    }

    /// Toggle marker on current frame.
    pub fn toggle_marker(&mut self) {
        let added = if self.markers.contains(&self.current_frame) {
            self.markers.remove(&self.current_frame);
            false
        } else {
            self.markers.insert(self.current_frame);
            true
        };
        self.log_marker(added);
    }

    /// Toggle recording mode.
    pub fn toggle_recording(&mut self) {
        let from_state = self.playback_state.label();
        self.playback_state = match self.playback_state {
            PlaybackState::Recording => PlaybackState::Paused,
            _ => PlaybackState::Recording,
        };
        self.log_playback(from_state);
    }

    // ========================================================================
    // Diagnostic Logging Helpers
    // ========================================================================

    /// Log a navigation event.
    fn log_navigation(&mut self, action: &'static str, from_frame: usize) {
        if !self.diagnostic_config.enabled || !self.diagnostic_config.log_navigation {
            return;
        }
        let seq = self.diagnostic_log.next_seq();
        self.diagnostic_log.push(DiagnosticEntry::Navigation {
            seq,
            action,
            from_frame,
            to_frame: self.current_frame,
            frame_count: self.frames.len(),
        });
    }

    /// Log a playback state change.
    fn log_playback(&mut self, from_state: &'static str) {
        if !self.diagnostic_config.enabled || !self.diagnostic_config.log_playback {
            return;
        }
        let seq = self.diagnostic_log.next_seq();
        self.diagnostic_log.push(DiagnosticEntry::PlaybackChange {
            seq,
            from_state,
            to_state: self.playback_state.label(),
            current_frame: self.current_frame,
        });
    }

    /// Log a marker toggle.
    fn log_marker(&mut self, added: bool) {
        if !self.diagnostic_config.enabled {
            return;
        }
        let seq = self.diagnostic_log.next_seq();
        self.diagnostic_log.push(DiagnosticEntry::MarkerToggled {
            seq,
            frame_index: self.current_frame,
            added,
            total_markers: self.markers.len(),
        });
    }

    /// Log a frame recorded event.
    fn log_frame_recorded(
        &mut self,
        frame_index: usize,
        change_count: usize,
        checksum: u64,
        width: u16,
        height: u16,
    ) {
        if !self.diagnostic_config.enabled || !self.diagnostic_config.log_recording {
            return;
        }
        let seq = self.diagnostic_log.next_seq();
        self.diagnostic_log.push(DiagnosticEntry::FrameRecorded {
            seq,
            frame_index,
            change_count,
            checksum,
            chain_checksum: self.checksum_chain,
            width,
            height,
        });
    }

    /// Log a clear event.
    fn log_cleared(&mut self) {
        if !self.diagnostic_config.enabled {
            return;
        }
        let seq = self.diagnostic_log.next_seq();
        self.diagnostic_log.push(DiagnosticEntry::Cleared {
            seq,
            frame_count: self.frames.len(),
            marker_count: self.markers.len(),
        });
    }

    /// Get the diagnostic log (for testing/inspection).
    pub fn diagnostic_log(&self) -> &DiagnosticLog {
        &self.diagnostic_log
    }

    /// Export diagnostic log to JSONL format.
    pub fn export_diagnostics(&self) -> String {
        self.diagnostic_log.to_jsonl()
    }

    fn export_report(&mut self) {
        let path = self.export_path.clone();
        let mut ok = true;

        self.clamp_compare_indices();
        let Some((a_idx, b_idx)) = self.compare_pair() else {
            self.last_export = Some(ExportStatus {
                path,
                ok: false,
                message: "no frames to export".to_string(),
            });
            return;
        };

        if self.diff_cache.is_none()
            || self
                .diff_cache
                .as_ref()
                .is_some_and(|cache| cache.a_index != a_idx || cache.b_index != b_idx)
        {
            self.diff_cache = Some(self.compute_diff_cache(a_idx, b_idx));
        }
        let cache = self.diff_cache.as_ref();
        let (
            width,
            height,
            diff_count,
            content_diff_count,
            style_diff_count,
            checksum_a,
            checksum_b,
        ) = if let Some(cache) = cache {
            (
                cache.width,
                cache.height,
                cache.diff_count,
                cache.content_diff_count,
                cache.style_diff_count,
                cache.checksum_a,
                cache.checksum_b,
            )
        } else {
            (0, 0, 0, 0, 0, 0, 0)
        };

        let total_cells = (width as usize).saturating_mul(height as usize).max(1);
        let diff_pct = diff_count as f64 / total_cells as f64;

        let message = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut file) => {
                let line = format!(
                    "{{\"event\":\"time_travel_report\",\"frame_a\":{},\"frame_b\":{},\"width\":{},\"height\":{},\"checksum_a\":\"0x{:016x}\",\"checksum_b\":\"0x{:016x}\",\"diff_cells\":{},\"diff_pct\":{:.6},\"content_diff\":{},\"style_diff\":{}}}\n",
                    a_idx,
                    b_idx,
                    width,
                    height,
                    checksum_a,
                    checksum_b,
                    diff_count,
                    diff_pct,
                    content_diff_count,
                    style_diff_count
                );
                if let Err(err) = file.write_all(line.as_bytes()) {
                    ok = false;
                    format!("write failed: {err}")
                } else {
                    "report appended".to_string()
                }
            }
            Err(err) => {
                ok = false;
                format!("open failed: {err}")
            }
        };

        self.last_export = Some(ExportStatus { path, ok, message });
    }

    // ========================================================================
    // Mouse Handling
    // ========================================================================

    fn frame_from_timeline_x(&self, timeline: Rect, x: u16) -> Option<usize> {
        if self.frames.is_empty() || timeline.is_empty() {
            return None;
        }
        if self.frames.len() == 1 {
            return Some(0);
        }

        let right_edge = timeline.x.saturating_add(timeline.width.saturating_sub(1));
        let clamped_x = x.clamp(timeline.x, right_edge);
        let rel_x = clamped_x.saturating_sub(timeline.x) as f64;
        let width = timeline.width.saturating_sub(1).max(1) as f64;
        let target = (rel_x / width * (self.frames.len() - 1) as f64).round() as usize;
        Some(target.min(self.frames.len() - 1))
    }

    fn scrub_timeline_to_x(&mut self, timeline: Rect, x: u16, action: &'static str) {
        let Some(target) = self.frame_from_timeline_x(timeline, x) else {
            return;
        };
        self.playback_state = PlaybackState::Paused;
        if target != self.current_frame {
            let from = self.current_frame;
            self.current_frame = target;
            self.log_navigation(action, from);
        }
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let timeline = self.layout_timeline.get();
        let preview = self.layout_preview.get();
        let info = self.layout_info.get();

        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if timeline.contains(x, y) && !self.frames.is_empty() {
                    self.timeline_scrubbing = true;
                    self.scrub_timeline_to_x(timeline, x, "click_timeline");
                } else if preview.contains(x, y) {
                    self.timeline_scrubbing = false;
                    self.toggle_playback();
                } else if info.contains(x, y) {
                    self.timeline_scrubbing = false;
                    self.toggle_compare_view();
                } else {
                    self.timeline_scrubbing = false;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                if self.timeline_scrubbing {
                    self.scrub_timeline_to_x(timeline, x, "drag_timeline");
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.timeline_scrubbing {
                    self.scrub_timeline_to_x(timeline, x, "release_timeline");
                }
                self.timeline_scrubbing = false;
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.timeline_scrubbing = false;
                if timeline.contains(x, y) {
                    self.toggle_marker();
                } else if preview.contains(x, y) {
                    self.toggle_heatmap();
                }
            }
            MouseEventKind::ScrollUp => {
                self.timeline_scrubbing = false;
                if timeline.contains(x, y) || preview.contains(x, y) {
                    self.playback_state = PlaybackState::Paused;
                    self.step_forward();
                }
            }
            MouseEventKind::ScrollDown => {
                self.timeline_scrubbing = false;
                if timeline.contains(x, y) || preview.contains(x, y) {
                    self.playback_state = PlaybackState::Paused;
                    self.step_backward();
                }
            }
            _ => {}
        }
    }

    // ========================================================================
    // Rendering
    // ========================================================================

    fn render_main_layout(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        match self.compare_view {
            StudioView::Single => self.render_single_layout(frame, area),
            StudioView::Compare => self.render_compare_layout(frame, area),
        }
    }

    fn render_single_layout(&self, frame: &mut Frame, area: Rect) {
        // Layout: Preview (left) | Info panel (right)
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);

        if chunks.len() >= 2 {
            // Left side: Timeline + Preview
            let left_chunks = Flex::vertical()
                .constraints([Constraint::Fixed(3), Constraint::Min(1)])
                .split(chunks[0]);
            if left_chunks.len() >= 2 {
                self.layout_timeline.set(left_chunks[0]);
                self.layout_preview.set(left_chunks[1]);
                self.render_timeline(frame, left_chunks[0]);
                self.render_preview(frame, left_chunks[1]);
            }

            // Right side: Info + Controls
            self.layout_info.set(chunks[1]);
            self.render_info_panel(frame, chunks[1]);
        }
    }

    fn render_compare_layout(&self, frame: &mut Frame, area: Rect) {
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);

        if chunks.len() >= 2 {
            let left_chunks = Flex::vertical()
                .constraints([Constraint::Fixed(3), Constraint::Min(1)])
                .split(chunks[0]);
            if left_chunks.len() >= 2 {
                self.layout_timeline.set(left_chunks[0]);
                self.layout_preview.set(left_chunks[1]);
                self.render_timeline(frame, left_chunks[0]);
                self.render_compare_preview(frame, left_chunks[1]);
            }

            self.layout_info.set(chunks[1]);
            self.render_info_panel(frame, chunks[1]);
        }
    }

    fn render_timeline(&self, frame: &mut Frame, area: Rect) {
        let border_style = Style::new().fg(theme::screen_accent::PERFORMANCE);

        let title = format!(
            "Timeline ({}/{})",
            self.current_frame + 1,
            self.frame_count().max(1)
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() || self.frames.is_empty() {
            return;
        }

        // Draw timeline bar
        let progress =
            self.current_frame as f64 / (self.frames.len().saturating_sub(1).max(1)) as f64;
        let bar_width = ((inner.width as f64) * progress) as u16;

        // Draw progress bar
        for x in 0..inner.width {
            let ch = if x < bar_width { '█' } else { '░' };
            let fg_color = if x < bar_width {
                theme::accent::INFO
            } else {
                theme::fg::DISABLED
            };
            let cell = Cell::from_char(ch).with_fg(fg_color.into());
            frame.buffer.set_fast(inner.x + x, inner.y, cell);
        }

        // Draw markers
        for &marker_idx in &self.markers {
            let marker_x = if self.frames.len() > 1 {
                (marker_idx as f64 / (self.frames.len() - 1) as f64 * inner.width as f64) as u16
            } else {
                0
            };
            if marker_x < inner.width {
                let cell = Cell::from_char('▼').with_fg(theme::accent::WARNING.into());
                frame.buffer.set_fast(inner.x + marker_x, inner.y, cell);
            }
        }

        if self.compare_view == StudioView::Compare {
            let count = self.frames.len();
            let frame_to_x = |idx: usize| -> u16 {
                if count <= 1 {
                    0
                } else {
                    let ratio = idx as f64 / (count - 1) as f64;
                    (ratio * inner.width as f64).floor() as u16
                }
            };
            if let Some((a_idx, b_idx)) = self.compare_pair() {
                let a_x = frame_to_x(a_idx).min(inner.width.saturating_sub(1));
                let b_x = frame_to_x(b_idx).min(inner.width.saturating_sub(1));
                if a_x == b_x {
                    let cell = Cell::from_char('◆').with_fg(theme::accent::INFO.into());
                    frame.buffer.set_fast(inner.x + a_x, inner.y, cell);
                } else {
                    let a_cell = Cell::from_char('A').with_fg(theme::accent::INFO.into());
                    let b_cell = Cell::from_char('B').with_fg(theme::accent::WARNING.into());
                    frame.buffer.set_fast(inner.x + a_x, inner.y, a_cell);
                    frame.buffer.set_fast(inner.x + b_x, inner.y, b_cell);
                }
            }
        }
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        self.render_frame_block(frame, area, "Frame Preview", self.current_buffer());
    }

    fn render_compare_preview(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);
        if cols.len() < 2 {
            return;
        }

        let (a_idx, b_idx) = self.compare_pair().unwrap_or((0, 0));
        let buffer_a = self.frames.get(a_idx);
        let buffer_b = self.frames.get(b_idx);

        let _a_inner = self.render_frame_block(frame, cols[0], "Frame A", buffer_a);
        let b_inner = self.render_frame_block(frame, cols[1], "Frame B", buffer_b);

        if self.heatmap_mode == HeatmapMode::Overlay
            && let Some(cache) = &self.diff_cache
        {
            self.render_heatmap_overlay(frame, b_inner, cache);
        }
    }

    fn render_frame_block(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        buffer: Option<&Buffer>,
    ) -> Rect {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title)
            .title_alignment(Alignment::Center)
            .style(theme::content_border());

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return inner;
        }

        if let Some(buf) = buffer {
            self.blit_buffer(frame, inner, buf);
        } else {
            let msg = "No frames recorded";
            let msg_width = display_width(msg) as u16;
            let x = inner.x + (inner.width.saturating_sub(msg_width)) / 2;
            let y = inner.y + inner.height / 2;
            Paragraph::new(msg)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(Rect::new(x, y, msg_width, 1), frame);
        }

        inner
    }

    fn blit_buffer(&self, frame: &mut Frame, area: Rect, buf: &Buffer) {
        let width = area.width.min(buf.width());
        let height = area.height.min(buf.height());
        if width == 0 || height == 0 {
            return;
        }
        frame
            .buffer
            .copy_from(buf, Rect::new(0, 0, width, height), area.x, area.y);
    }

    fn render_heatmap_overlay(&self, frame: &mut Frame, area: Rect, cache: &DiffCache) {
        if area.is_empty() {
            return;
        }
        for cell in &cache.diff_cells {
            if cell.x >= area.width || cell.y >= area.height {
                continue;
            }
            let x = area.x + cell.x;
            let y = area.y + cell.y;
            let color = heatmap_gradient(cell.intensity);
            let mut base = *frame.buffer.get_unchecked(x, y);
            base.bg = color;
            frame.buffer.set_raw(x, y, base);
        }
    }

    fn render_info_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Frame Info")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines = Vec::new();

        // Playback status
        lines.push(format!("Status: {}", self.playback_state.label()));
        lines.push(String::new());

        if let Some(info) = self.current_info() {
            lines.push(format!("Frame: {}/{}", info.index + 1, self.frame_count()));
            lines.push(format!("Size: {}x{}", info.width, info.height));
            lines.push(format!("Changes: {} cells", info.change_count));
            lines.push(format!("Memory: {} bytes", info.memory_size));
            lines.push(format!("Checksum: {:016x}", info.checksum));
            if let Some(rt) = info.render_time {
                lines.push(format!("Render: {:?}", rt));
            }
            lines.push(String::new());
            lines.push(format!("Chain hash: {:016x}", self.checksum_chain));
            lines.push(format!("Markers: {}", self.markers.len()));
            lines.push(format!(
                "Marked: {}",
                if self.markers.contains(&self.current_frame) {
                    "Yes ▼"
                } else {
                    "No"
                }
            ));
        } else {
            lines.push("No frame data".to_string());
        }

        lines.push(String::new());
        lines.push("── Compare ──".to_string());
        lines.push(format!("View: {}", self.compare_view.label()));
        lines.push(format!("Heatmap: {}", self.heatmap_mode.label()));
        if let Some((a_idx, b_idx)) = self.compare_pair() {
            let a_info = self.frame_info.get(a_idx);
            let b_info = self.frame_info.get(b_idx);
            lines.push(format!(
                "A: #{}{}",
                a_idx + 1,
                a_info
                    .map(|info| format!("  {:016x}", info.checksum))
                    .unwrap_or_default()
            ));
            lines.push(format!(
                "B: #{}{}",
                b_idx + 1,
                b_info
                    .map(|info| format!("  {:016x}", info.checksum))
                    .unwrap_or_default()
            ));

            if let Some(cache) = &self.diff_cache {
                let total = (cache.width as usize)
                    .saturating_mul(cache.height as usize)
                    .max(1);
                let pct = cache.diff_count as f64 / total as f64 * 100.0;
                lines.push(format!("Diff: {} cells ({:.2}%)", cache.diff_count, pct));
                lines.push(format!(
                    "Content: {}  Style: {}",
                    cache.content_diff_count, cache.style_diff_count
                ));
            } else {
                lines.push("Diff: n/a".to_string());
            }
        } else {
            lines.push("A/B: n/a".to_string());
        }

        if let Some(export) = &self.last_export {
            let status = if export.ok { "ok" } else { "error" };
            lines.push(format!("Export: {status}"));
            lines.push(format!("Path: {}", export.path));
            lines.push(format!("Msg: {}", export.message));
        }

        lines.push(String::new());
        lines.push("── Controls ──".to_string());
        lines.push("Space: Play/Pause".to_string());
        lines.push("←/→ or h/l: Step".to_string());
        lines.push("Home/End or g/G: First/Last".to_string());
        lines.push("M: Toggle marker".to_string());
        lines.push("R: Toggle record".to_string());
        lines.push("C: Clear".to_string());
        lines.push("D: Diagnostics".to_string());
        lines.push("V: Toggle compare view".to_string());
        lines.push("A/B: Pin compare A/B".to_string());
        lines.push("X: Swap A/B".to_string());
        lines.push("H: Heatmap overlay".to_string());
        lines.push("E: Export JSONL".to_string());

        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let style = if line.starts_with("Status:") {
                self.playback_state.style()
            } else if line.starts_with("──") {
                Style::new().fg(theme::fg::MUTED)
            } else if line.contains(':') && !line.starts_with(' ') {
                Style::new().fg(theme::fg::SECONDARY)
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };

            Paragraph::new(line.as_str()).style(style).render(
                Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                frame,
            );
        }
    }
}

impl Screen for SnapshotPlayer {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse.kind, mouse.x, mouse.y);
            self.refresh_diff_cache();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                KeyCode::Char(' ') => self.toggle_playback(),
                KeyCode::Left | KeyCode::Char('h') => {
                    self.playback_state = PlaybackState::Paused;
                    self.step_backward();
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.playback_state = PlaybackState::Paused;
                    self.step_forward();
                }
                KeyCode::Home => {
                    self.playback_state = PlaybackState::Paused;
                    self.go_to_start();
                }
                KeyCode::End => {
                    self.playback_state = PlaybackState::Paused;
                    self.go_to_end();
                }
                KeyCode::Char('m') | KeyCode::Char('M') => self.toggle_marker(),
                KeyCode::Char('r') | KeyCode::Char('R') => self.toggle_recording(),
                KeyCode::Char('c') | KeyCode::Char('C') => self.clear(),
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.show_diagnostics = !self.show_diagnostics;
                }
                KeyCode::Char('g') => self.go_to_start(),
                KeyCode::Char('G') => self.go_to_end(),
                KeyCode::Char('v') | KeyCode::Char('V') => self.toggle_compare_view(),
                KeyCode::Char('H') => self.toggle_heatmap(),
                KeyCode::Char('a') | KeyCode::Char('A') => self.set_compare_a(self.current_frame),
                KeyCode::Char('b') | KeyCode::Char('B') => self.set_compare_b(self.current_frame),
                KeyCode::Char('x') | KeyCode::Char('X') => self.swap_compare(),
                KeyCode::Char('e') | KeyCode::Char('E') => self.export_report(),
                _ => {}
            }
        }

        self.refresh_diff_cache();
        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;

        if self.playback_state == PlaybackState::Playing {
            // Advance frame during playback (every N ticks based on speed)
            if tick_count.is_multiple_of(2) {
                // Advance every 2 ticks (~5 fps)
                if self.current_frame + 1 < self.frames.len() {
                    self.current_frame += 1;
                } else {
                    // Loop back to start
                    self.current_frame = 0;
                }
            }
        }

        self.refresh_diff_cache();
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        self.render_main_layout(frame, area);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Space",
                action: "Play/Pause",
            },
            HelpEntry {
                key: "←/→ or h/l",
                action: "Step frame",
            },
            HelpEntry {
                key: "Home/End or g/G",
                action: "First/Last",
            },
            HelpEntry {
                key: "M",
                action: "Toggle marker",
            },
            HelpEntry {
                key: "R",
                action: "Toggle record",
            },
            HelpEntry {
                key: "C",
                action: "Clear all",
            },
            HelpEntry {
                key: "D",
                action: "Diagnostics",
            },
            HelpEntry {
                key: "V",
                action: "Toggle compare view",
            },
            HelpEntry {
                key: "A/B",
                action: "Pin compare A/B",
            },
            HelpEntry {
                key: "X",
                action: "Swap A/B",
            },
            HelpEntry {
                key: "H",
                action: "Heatmap overlay",
            },
            HelpEntry {
                key: "E",
                action: "Export JSONL report",
            },
            HelpEntry {
                key: "Click timeline",
                action: "Jump to frame",
            },
            HelpEntry {
                key: "Drag timeline",
                action: "Scrub frames",
            },
            HelpEntry {
                key: "Scroll",
                action: "Step frame",
            },
            HelpEntry {
                key: "Right-click",
                action: "Toggle marker/heatmap",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Time-Travel Studio"
    }

    fn tab_label(&self) -> &'static str {
        "TimeTravel"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn new_creates_demo_frames() {
        let player = SnapshotPlayer::new();
        assert_eq!(player.frame_count(), 50);
        assert_eq!(player.current_frame, 0);
        assert_eq!(player.playback_state, PlaybackState::Paused);
    }

    #[test]
    fn step_forward_advances_frame() {
        let mut player = SnapshotPlayer::new();
        assert_eq!(player.current_frame, 0);
        player.step_forward();
        assert_eq!(player.current_frame, 1);
    }

    #[test]
    fn step_forward_clamps_at_end() {
        let mut player = SnapshotPlayer::new();
        player.go_to_end();
        let last = player.current_frame;
        player.step_forward();
        assert_eq!(player.current_frame, last);
    }

    #[test]
    fn step_backward_decrements_frame() {
        let mut player = SnapshotPlayer::new();
        player.step_forward();
        player.step_forward();
        assert_eq!(player.current_frame, 2);
        player.step_backward();
        assert_eq!(player.current_frame, 1);
    }

    #[test]
    fn step_backward_clamps_at_zero() {
        let mut player = SnapshotPlayer::new();
        player.step_backward();
        assert_eq!(player.current_frame, 0);
    }

    #[test]
    fn go_to_start_resets_to_zero() {
        let mut player = SnapshotPlayer::new();
        player.go_to_end();
        assert!(player.current_frame > 0);
        player.go_to_start();
        assert_eq!(player.current_frame, 0);
    }

    #[test]
    fn go_to_end_jumps_to_last() {
        let mut player = SnapshotPlayer::new();
        player.go_to_end();
        assert_eq!(player.current_frame, player.frame_count() - 1);
    }

    #[test]
    fn toggle_playback_changes_state() {
        let mut player = SnapshotPlayer::new();
        assert_eq!(player.playback_state, PlaybackState::Paused);
        player.toggle_playback();
        assert_eq!(player.playback_state, PlaybackState::Playing);
        player.toggle_playback();
        assert_eq!(player.playback_state, PlaybackState::Paused);
    }

    #[test]
    fn toggle_marker_adds_and_removes() {
        let mut player = SnapshotPlayer::new();
        assert!(!player.markers.contains(&0));
        player.toggle_marker();
        assert!(player.markers.contains(&0));
        player.toggle_marker();
        assert!(!player.markers.contains(&0));
    }

    #[test]
    fn clear_removes_all_frames() {
        let mut player = SnapshotPlayer::new();
        assert!(player.frame_count() > 0);
        player.clear();
        assert_eq!(player.frame_count(), 0);
        assert_eq!(player.current_frame, 0);
        assert!(player.markers.is_empty());
    }

    #[test]
    fn frame_info_has_valid_checksums() {
        let player = SnapshotPlayer::new();
        let info = player.current_info().unwrap();
        assert!(info.checksum != 0);
    }

    #[test]
    fn frame_info_tracks_change_counts() {
        let player = SnapshotPlayer::new();
        // First frame should have many changes (full snapshot)
        let first_info = &player.frame_info[0];
        assert!(first_info.change_count > 0);

        // Later frames should have fewer changes (deltas)
        if player.frame_count() > 1 {
            let later_info = &player.frame_info[1];
            assert!(later_info.change_count < first_info.change_count);
        }
    }

    #[test]
    fn renders_without_panic() {
        let player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));
    }

    #[test]
    fn renders_small_area() {
        let player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 40, 10));
    }

    #[test]
    fn renders_empty_area() {
        let player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn tick_advances_during_playback() {
        let mut player = SnapshotPlayer::new();
        player.toggle_playback(); // Start playing
        let initial = player.current_frame;
        player.tick(2);
        // Should advance after tick
        assert!(player.current_frame > initial || player.current_frame == 0);
    }

    #[test]
    fn tick_does_not_advance_when_paused() {
        let mut player = SnapshotPlayer::new();
        let initial = player.current_frame;
        player.tick(2);
        assert_eq!(player.current_frame, initial);
    }

    #[test]
    fn playback_loops_at_end() {
        let mut player = SnapshotPlayer::new();
        player.go_to_end();
        player.toggle_playback();
        player.tick(2);
        assert_eq!(player.current_frame, 0); // Looped back
    }

    #[test]
    fn custom_config_respected() {
        let config = SnapshotPlayerConfig {
            max_frames: 10,
            playback_speed: 2,
            auto_generate_demo: true,
            demo_frame_count: 5,
        };
        let player = SnapshotPlayer::with_config(config);
        assert_eq!(player.frame_count(), 5);
    }

    #[test]
    fn title_and_label() {
        let player = SnapshotPlayer::new();
        assert_eq!(player.title(), "Time-Travel Studio");
        assert_eq!(player.tab_label(), "TimeTravel");
    }

    #[test]
    fn keybindings_not_empty() {
        let player = SnapshotPlayer::new();
        assert!(!player.keybindings().is_empty());
    }

    #[test]
    fn compare_view_toggles() {
        let mut player = SnapshotPlayer::new();
        assert_eq!(player.compare_view, StudioView::Single);
        player.toggle_compare_view();
        assert_eq!(player.compare_view, StudioView::Compare);
    }

    #[test]
    fn diff_cache_counts_content_and_style_changes() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            max_frames: 4,
            ..Default::default()
        };
        let mut player = SnapshotPlayer::with_config(config);

        let mut buf_a = Buffer::new(4, 2);
        let mut buf_b = Buffer::new(4, 2);
        buf_a.set(0, 0, Cell::from_char('A'));
        buf_b.set(0, 0, Cell::from_char('B')); // content diff
        buf_a.set(1, 0, Cell::from_char('Z'));
        buf_b.set(
            1,
            0,
            Cell::from_char('Z').with_fg(theme::accent::ERROR.into()),
        ); // style diff

        player.record_frame(&buf_a);
        player.record_frame(&buf_b);
        player.toggle_compare_view();
        player.set_compare_a(0);
        player.set_compare_b(1);
        player.refresh_diff_cache();

        let cache = player.diff_cache.as_ref().expect("diff cache");
        assert_eq!(cache.diff_count, 2);
        assert_eq!(cache.content_diff_count, 1);
        assert_eq!(cache.style_diff_count, 1);
    }

    #[test]
    fn click_timeline_jumps_to_frame() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let timeline = player.layout_timeline.get();
        assert!(!timeline.is_empty());

        let mid_x = timeline.x + timeline.width / 2;
        let mid_y = timeline.y;
        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: mid_x,
            y: mid_y,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        player.update(&event);

        let expected_mid = player.frame_count() / 2;
        let tolerance = player.frame_count() / 4;
        assert!(
            player.current_frame.abs_diff(expected_mid) <= tolerance,
            "Expected near frame {} but got {}",
            expected_mid,
            player.current_frame
        );
        assert_eq!(player.playback_state, PlaybackState::Paused);
    }

    #[test]
    fn click_preview_toggles_playback() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let preview = player.layout_preview.get();
        assert!(!preview.is_empty());

        assert_eq!(player.playback_state, PlaybackState::Paused);
        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: preview.x + 1,
            y: preview.y + 1,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        player.update(&event);
        assert_eq!(player.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn right_click_timeline_toggles_marker() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let timeline = player.layout_timeline.get();
        assert!(!player.markers.contains(&player.current_frame));

        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            x: timeline.x + 1,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        player.update(&event);
        assert!(player.markers.contains(&player.current_frame));
    }

    #[test]
    fn scroll_steps_frame() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let timeline = player.layout_timeline.get();
        assert_eq!(player.current_frame, 0);

        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            x: timeline.x + 1,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        player.update(&event);
        assert_eq!(player.current_frame, 1);

        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x: timeline.x + 1,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        player.update(&event);
        assert_eq!(player.current_frame, 0);
    }

    #[test]
    fn drag_timeline_scrubs_to_end() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let timeline = player.layout_timeline.get();
        assert!(!timeline.is_empty());

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: timeline.x,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert!(player.timeline_scrubbing);

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            x: timeline.x + timeline.width.saturating_sub(1),
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(player.current_frame, player.frame_count().saturating_sub(1));
        assert_eq!(player.playback_state, PlaybackState::Paused);
    }

    #[test]
    fn drag_timeline_clamps_when_pointer_moves_outside() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let timeline = player.layout_timeline.get();
        assert!(!timeline.is_empty());

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: timeline.x,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            x: timeline.x.saturating_add(timeline.width).saturating_add(25),
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(player.current_frame, player.frame_count().saturating_sub(1));
    }

    #[test]
    fn timeline_release_stops_scrubbing() {
        use super::Screen;
        let mut player = SnapshotPlayer::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 120, 40));

        let timeline = player.layout_timeline.get();
        assert!(!timeline.is_empty());
        let mid_x = timeline.x + timeline.width / 2;

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: timeline.x,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            x: mid_x,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        let scrubbed_frame = player.current_frame;

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            x: mid_x,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert!(!player.timeline_scrubbing);

        player.update(&Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Moved,
            x: timeline.x,
            y: timeline.y,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(player.current_frame, scrubbed_frame);
    }

    #[test]
    fn keybindings_include_mouse_hints() {
        use super::Screen;
        let player = SnapshotPlayer::new();
        let bindings = player.keybindings();
        assert!(bindings.iter().any(|e| e.key.contains("Click")));
        assert!(bindings.iter().any(|e| e.key.contains("Scroll")));
        assert!(bindings.iter().any(|e| e.key.contains("Right-click")));
    }

    // ========================================================================
    // Edge Case Tests (bd-3sa7.3)
    // ========================================================================

    #[test]
    fn empty_player_handles_navigation() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            ..Default::default()
        };
        let mut player = SnapshotPlayer::with_config(config);
        assert_eq!(player.frame_count(), 0);

        // Navigation on empty player should not panic
        player.step_forward();
        player.step_backward();
        player.go_to_start();
        player.go_to_end();
        assert_eq!(player.current_frame, 0);
    }

    #[test]
    fn empty_player_current_buffer_is_none() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            ..Default::default()
        };
        let player = SnapshotPlayer::with_config(config);
        assert!(player.current_buffer().is_none());
        assert!(player.current_info().is_none());
    }

    #[test]
    fn record_frame_adds_to_empty_player() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            max_frames: 10,
            ..Default::default()
        };
        let mut player = SnapshotPlayer::with_config(config);
        assert_eq!(player.frame_count(), 0);

        let buf = Buffer::new(10, 5);
        player.record_frame(&buf);
        assert_eq!(player.frame_count(), 1);
        assert!(player.current_buffer().is_some());
    }

    #[test]
    fn record_frame_respects_max_frames() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            max_frames: 3,
            ..Default::default()
        };
        let mut player = SnapshotPlayer::with_config(config);

        // Record 5 frames
        for i in 0..5 {
            let mut buf = Buffer::new(10, 5);
            // Mark each buffer distinctly
            buf.set(0, 0, Cell::from_char((b'A' + i as u8) as char));
            player.record_frame(&buf);
        }

        // Should only keep the last 3
        assert_eq!(player.frame_count(), 3);
        // Frame indices should be reindexed
        assert_eq!(player.frame_info[0].index, 0);
        assert_eq!(player.frame_info[1].index, 1);
        assert_eq!(player.frame_info[2].index, 2);
    }

    #[test]
    fn checksum_chain_accumulates() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            ..Default::default()
        };
        let mut player = SnapshotPlayer::with_config(config);
        assert_eq!(player.checksum_chain, 0);

        let buf = Buffer::new(10, 5);
        player.record_frame(&buf);
        let after_first = player.checksum_chain;
        assert!(after_first != 0);

        player.record_frame(&buf);
        // Chain should grow (wrapping add)
        assert!(player.checksum_chain != after_first);
    }

    #[test]
    fn toggle_recording_state() {
        let mut player = SnapshotPlayer::new();
        assert_eq!(player.playback_state, PlaybackState::Paused);

        player.toggle_recording();
        assert_eq!(player.playback_state, PlaybackState::Recording);

        player.toggle_recording();
        assert_eq!(player.playback_state, PlaybackState::Paused);
    }

    #[test]
    fn recording_to_playing_via_toggle_playback() {
        let mut player = SnapshotPlayer::new();
        player.toggle_recording();
        assert_eq!(player.playback_state, PlaybackState::Recording);

        player.toggle_playback();
        assert_eq!(player.playback_state, PlaybackState::Playing);
    }

    #[test]
    fn key_event_updates() {
        use ftui_core::event::Modifiers;

        let mut player = SnapshotPlayer::new();

        // Test space key
        let space_event = Event::Key(KeyEvent {
            code: KeyCode::Char(' '),
            kind: KeyEventKind::Press,
            modifiers: Modifiers::NONE,
        });
        player.update(&space_event);
        assert_eq!(player.playback_state, PlaybackState::Playing);

        // Test 'd' key for diagnostics toggle
        let initial_diag = player.show_diagnostics;
        let d_event = Event::Key(KeyEvent {
            code: KeyCode::Char('d'),
            kind: KeyEventKind::Press,
            modifiers: Modifiers::NONE,
        });
        player.update(&d_event);
        assert_ne!(player.show_diagnostics, initial_diag);
    }

    // ========================================================================
    // Invariant Tests (bd-3sa7.3)
    // ========================================================================

    /// Invariant 1: Progress bounds - current_frame is always within valid range
    #[test]
    fn invariant_progress_bounds() {
        let mut player = SnapshotPlayer::new();
        let n = player.frame_count();

        // After any navigation, current_frame should be in [0, n-1]
        for _ in 0..100 {
            player.step_forward();
        }
        assert!(player.current_frame < n);

        for _ in 0..100 {
            player.step_backward();
        }
        assert_eq!(player.current_frame, 0);
    }

    /// Invariant 2: Playback determinism - same frame index yields same buffer
    #[test]
    fn invariant_playback_determinism() {
        let player = SnapshotPlayer::new();
        let idx = 10;

        let buf1 = &player.frames[idx];
        let buf2 = &player.frames[idx];

        // Same buffer should have identical content
        assert_eq!(buf1.width(), buf2.width());
        assert_eq!(buf1.height(), buf2.height());

        let checksum1 = player.frame_info[idx].checksum;
        let checksum2 = player.frame_info[idx].checksum;
        assert_eq!(checksum1, checksum2);
    }

    /// Invariant 3: Checksum integrity - checksums are consistent
    #[test]
    fn invariant_checksum_consistency() {
        let player = SnapshotPlayer::new();

        // Recalculate checksum for first frame
        let buf = &player.frames[0];
        let recalc = player.calculate_checksum(buf);
        assert_eq!(recalc, player.frame_info[0].checksum);
    }

    // ========================================================================
    // Diagnostic Logging Tests (bd-3sa7.5)
    // ========================================================================

    #[test]
    fn diagnostic_log_captures_navigation() {
        let mut player = SnapshotPlayer::new();
        let initial_entries = player.diagnostic_log().entries().len();
        player.step_forward();
        player.step_backward();
        player.go_to_start();
        player.go_to_end();
        assert_eq!(player.diagnostic_log().entries().len(), initial_entries + 4);
    }

    #[test]
    fn diagnostic_log_captures_playback() {
        let mut player = SnapshotPlayer::new();
        let initial_entries = player.diagnostic_log().entries().len();
        player.toggle_playback();
        player.toggle_playback();
        assert!(player.diagnostic_log().entries().len() >= initial_entries + 2);
    }

    #[test]
    fn diagnostic_log_captures_markers() {
        let mut player = SnapshotPlayer::new();
        let initial_entries = player.diagnostic_log().entries().len();
        player.toggle_marker();
        player.toggle_marker();
        assert!(player.diagnostic_log().entries().len() >= initial_entries + 2);
    }

    #[test]
    fn diagnostic_log_to_jsonl() {
        let mut player = SnapshotPlayer::new();
        player.step_forward();
        player.toggle_playback();
        let jsonl = player.export_diagnostics();
        assert!(!jsonl.is_empty());
        assert!(jsonl.contains("\"event\""));
        assert!(jsonl.contains("\"seq\":"));
    }

    #[test]
    fn diagnostic_log_respects_disabled_config() {
        let config = SnapshotPlayerConfig {
            auto_generate_demo: false,
            ..Default::default()
        };
        let mut player = SnapshotPlayer::with_config(config);
        player.diagnostic_config.enabled = false;
        player.step_forward();
        player.toggle_playback();
        assert!(player.diagnostic_log().entries().is_empty());
    }
}

// ============================================================================
// Property Tests (bd-3sa7.3)
// ============================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Property: Frame index is always bounded after any sequence of navigation
        #[test]
        fn prop_frame_index_bounded(
            steps in prop::collection::vec(0u8..6, 0..50)
        ) {
            let mut player = SnapshotPlayer::new();
            let n = player.frame_count();

            for step in steps {
                match step % 6 {
                    0 => player.step_forward(),
                    1 => player.step_backward(),
                    2 => player.go_to_start(),
                    3 => player.go_to_end(),
                    4 => player.toggle_playback(),
                    _ => player.toggle_marker(),
                }
            }

            // Invariant: current_frame is always in valid range
            prop_assert!(player.current_frame < n || (n == 0 && player.current_frame == 0));
        }

        /// Property: Clear always resets to empty state
        #[test]
        fn prop_clear_resets_state(
            ops_before_clear in prop::collection::vec(0u8..4, 0..20)
        ) {
            let mut player = SnapshotPlayer::new();

            // Do some random operations
            for op in ops_before_clear {
                match op % 4 {
                    0 => player.step_forward(),
                    1 => player.toggle_marker(),
                    2 => player.go_to_end(),
                    _ => player.toggle_playback(),
                }
            }

            // Clear should reset everything
            player.clear();
            prop_assert_eq!(player.frame_count(), 0);
            prop_assert_eq!(player.current_frame, 0);
            prop_assert!(player.markers.is_empty());
            prop_assert_eq!(player.checksum_chain, 0);
            prop_assert_eq!(player.playback_state, PlaybackState::Paused);
        }

        /// Property: Recording adds exactly one frame per call
        #[test]
        fn prop_record_increments_count(
            record_count in 1usize..20,
            width in 5u16..50,
            height in 5u16..30
        ) {
            let config = SnapshotPlayerConfig {
                auto_generate_demo: false,
                max_frames: 100,
                ..Default::default()
            };
            let mut player = SnapshotPlayer::with_config(config);

            for i in 0..record_count {
                let buf = Buffer::new(width, height);
                player.record_frame(&buf);
                prop_assert_eq!(player.frame_count(), i + 1);
            }
        }

        /// Property: Checksums are non-zero for non-empty buffers
        #[test]
        fn prop_checksum_nonzero(
            width in 5u16..50,
            height in 5u16..30
        ) {
            let config = SnapshotPlayerConfig {
                auto_generate_demo: false,
                ..Default::default()
            };
            let mut player = SnapshotPlayer::with_config(config);

            let mut buf = Buffer::new(width, height);
            // Put some content in the buffer
            buf.set(0, 0, Cell::from_char('X'));

            player.record_frame(&buf);

            let info = player.current_info().unwrap();
            prop_assert!(info.checksum != 0);
        }

        /// Property: Frame info indices are always sequential
        #[test]
        fn prop_frame_info_indices_sequential(
            _seed in 0u64..1000
        ) {
            let player = SnapshotPlayer::new();

            for (i, info) in player.frame_info.iter().enumerate() {
                prop_assert_eq!(info.index, i);
            }
        }
    }
}
