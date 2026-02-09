#![forbid(unsafe_code)]

//! Scrollback viewport virtualization and smooth scroll physics.
//!
//! This module manages the scroll offset through a scrollback buffer, translates
//! wheel/touch deltas into viewport positions, and provides smooth (optionally
//! inertial) scroll physics. It does **not** own the scrollback data — it only
//! computes which lines to render and at what sub-pixel offset.
//!
//! # Design
//!
//! - [`ScrollConfig`] holds tuning parameters (overscan, speed, friction, …).
//! - [`ScrollState`] is the mutable scroll position. It consumes
//!   [`WheelInput`](crate::input::WheelInput) events and produces a
//!   [`ViewportSnapshot`] each frame.
//! - [`WheelCoalescer`] accumulates wheel deltas within a single frame tick so
//!   that high-frequency trackpad events are batched into one scroll step.
//!
//! The renderer calls [`ScrollState::viewport`] once per frame, passing the
//! current scrollback length and terminal row count, and receives a
//! [`ViewportSnapshot`] that tells it exactly which scrollback lines (plus
//! overscan) to fetch.

use crate::input::WheelInput;
use unicode_normalization::UnicodeNormalization;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tuning knobs for scroll behavior.
#[derive(Debug, Clone)]
pub struct ScrollConfig {
    /// Extra lines rendered above/below the visible viewport to reduce flicker
    /// during fast scrolling.
    pub overscan_lines: usize,

    /// Lines scrolled per discrete wheel tick (integer delta ±1).
    pub lines_per_tick: usize,

    /// Multiplier for high-resolution (pixel-mode) trackpad deltas.
    /// `dy * pixel_scale` is accumulated until it exceeds one line.
    pub pixel_scale: f64,

    /// Enable momentum / inertial scrolling after the last wheel event.
    pub inertia_enabled: bool,

    /// Per-frame velocity decay factor (0.0 = instant stop, 1.0 = no friction).
    /// Typical: 0.92–0.96.
    pub inertia_friction: f64,

    /// Velocity below which inertia snaps to zero (lines/frame).
    pub inertia_stop_threshold: f64,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            overscan_lines: 3,
            lines_per_tick: 3,
            pixel_scale: 1.0 / 40.0, // 40 px ≈ 1 line
            inertia_enabled: true,
            inertia_friction: 0.93,
            inertia_stop_threshold: 0.05,
        }
    }
}

// ---------------------------------------------------------------------------
// Viewport snapshot
// ---------------------------------------------------------------------------

/// Immutable snapshot of which scrollback region to render for this frame.
///
/// All line indexes are in scrollback space (0 = oldest stored line).
/// The renderer should fetch lines in `render_start..render_end` and position
/// the viewport starting at `viewport_start`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportSnapshot {
    /// Total lines currently in the scrollback buffer.
    pub total_lines: usize,

    /// Visible viewport start (inclusive, scrollback index).
    pub viewport_start: usize,
    /// Visible viewport end (exclusive, scrollback index).
    pub viewport_end: usize,

    /// Render region start including overscan (inclusive).
    pub render_start: usize,
    /// Render region end including overscan (exclusive).
    pub render_end: usize,

    /// Sub-line pixel offset for smooth scrolling (0.0–1.0).
    /// 0.0 = top-aligned on `viewport_start`.
    pub sub_line_offset: f64,

    /// Discrete line offset from the bottom (0 = newest).
    pub scroll_offset_from_bottom: usize,
    /// Maximum legal scroll offset.
    pub max_scroll_offset: usize,

    /// Whether the viewport is at the very bottom (tracking live output).
    pub is_at_bottom: bool,

    /// Whether inertial animation is still running (renderer should
    /// request another frame).
    pub is_animating: bool,
}

impl ViewportSnapshot {
    /// Number of visible viewport lines.
    #[must_use]
    pub fn viewport_len(&self) -> usize {
        self.viewport_end.saturating_sub(self.viewport_start)
    }

    /// Number of lines in the render range (viewport + overscan).
    #[must_use]
    pub fn render_len(&self) -> usize {
        self.render_end.saturating_sub(self.render_start)
    }
}

// ---------------------------------------------------------------------------
// Wheel event coalescer
// ---------------------------------------------------------------------------

/// Accumulates multiple wheel events within a single frame into one delta.
///
/// High-frequency trackpad events can produce 10+ events between frames.
/// The coalescer sums them so [`ScrollState`] applies one scroll step per
/// frame tick.
#[derive(Debug, Clone, Default)]
pub struct WheelCoalescer {
    /// Accumulated vertical delta (positive = scroll up / toward older lines).
    accumulated_dy: i32,
    /// Number of events coalesced this frame.
    event_count: u32,
}

impl WheelCoalescer {
    /// Create a new coalescer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a wheel event. Call this for every `InputEvent::Wheel` received.
    pub fn push(&mut self, wheel: &WheelInput) {
        self.accumulated_dy = self.accumulated_dy.saturating_add(i32::from(wheel.dy));
        self.event_count = self.event_count.saturating_add(1);
    }

    /// Drain the accumulated delta and reset for the next frame.
    ///
    /// Returns `(total_dy, event_count)`.
    pub fn drain(&mut self) -> (i32, u32) {
        let result = (self.accumulated_dy, self.event_count);
        self.accumulated_dy = 0;
        self.event_count = 0;
        result
    }

    /// Whether any events were accumulated since the last drain.
    #[must_use]
    pub fn has_events(&self) -> bool {
        self.event_count > 0
    }
}

// ---------------------------------------------------------------------------
// Scroll state
// ---------------------------------------------------------------------------

/// Mutable scroll state for a single terminal instance.
///
/// Owns the scroll offset and inertial velocity. Each frame:
/// 1. Feed wheel events via the [`WheelCoalescer`].
/// 2. Call [`ScrollState::apply_wheel`] with the coalesced delta.
/// 3. Call [`ScrollState::tick`] to advance inertia (if enabled).
/// 4. Call [`ScrollState::viewport`] to get the [`ViewportSnapshot`].
#[derive(Debug, Clone)]
pub struct ScrollState {
    /// Discrete line offset from bottom (0 = newest, increases toward oldest).
    offset: usize,

    /// Fractional sub-line accumulator for pixel-mode trackpad deltas.
    /// When `|fractional| >= 1.0`, a whole line is consumed.
    fractional: f64,

    /// Inertial velocity in lines-per-frame (positive = scroll up).
    velocity: f64,

    /// Whether inertia is currently animating.
    animating: bool,

    /// Configuration.
    config: ScrollConfig,
}

impl ScrollState {
    /// Create a new scroll state at the bottom of the scrollback.
    #[must_use]
    pub fn new(config: ScrollConfig) -> Self {
        Self {
            offset: 0,
            fractional: 0.0,
            velocity: 0.0,
            animating: false,
            config,
        }
    }

    /// Create a scroll state with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(ScrollConfig::default())
    }

    /// Current discrete scroll offset from bottom.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Whether inertial scrolling is animating.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    /// Replace the scroll configuration.
    pub fn set_config(&mut self, config: ScrollConfig) {
        let disabling_inertia = self.config.inertia_enabled && !config.inertia_enabled;
        self.config = config;
        if disabling_inertia {
            // If inertia is turned off at runtime, stop any in-flight momentum
            // immediately so behavior matches the new configuration.
            self.velocity = 0.0;
            self.fractional = 0.0;
            self.animating = false;
        }
    }

    /// Jump to a specific offset from bottom (clamped on next viewport call).
    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
        self.velocity = 0.0;
        self.fractional = 0.0;
        self.animating = false;
    }

    /// Jump to the bottom (newest output). Stops any inertia.
    pub fn snap_to_bottom(&mut self) {
        self.set_offset(0);
    }

    /// Jump to the top (oldest stored line). Stops any inertia.
    pub fn snap_to_top(&mut self, total_scrollback_lines: usize, viewport_rows: usize) {
        let max = total_scrollback_lines.saturating_sub(viewport_rows);
        self.set_offset(max);
    }

    /// Jump the viewport to a specific scrollback line.
    ///
    /// The target line is clamped into valid bounds and placed near the center
    /// of the viewport when possible.
    pub fn jump_to_line(
        &mut self,
        total_scrollback_lines: usize,
        viewport_rows: usize,
        target_line: usize,
    ) {
        let viewport_len = viewport_rows.min(total_scrollback_lines);
        if viewport_len == 0 || total_scrollback_lines == 0 {
            self.set_offset(0);
            return;
        }

        let max_start = total_scrollback_lines.saturating_sub(viewport_len);
        let newest_start = max_start;
        let clamped_target = target_line.min(total_scrollback_lines.saturating_sub(1));
        let half = viewport_len / 2;
        let target_start = clamped_target.saturating_sub(half).min(max_start);

        self.set_offset(newest_start.saturating_sub(target_start));
    }

    /// Scroll by a signed number of lines (positive = older, negative = newer).
    pub fn scroll_lines(&mut self, delta: isize) {
        if delta >= 0 {
            self.offset = self.offset.saturating_add(delta as usize);
        } else {
            self.offset = self.offset.saturating_sub((-delta) as usize);
        }
        self.velocity = 0.0;
        self.fractional = 0.0;
        self.animating = false;
    }

    /// Apply a coalesced wheel delta.
    ///
    /// `total_dy` is the sum of `WheelInput::dy` values coalesced this frame.
    /// Negative dy = scroll down (toward newer), positive dy = scroll up (toward
    /// older), matching the "natural" convention.
    pub fn apply_wheel(&mut self, total_dy: i32, max_offset: usize) {
        if total_dy == 0 {
            return;
        }
        let previous_offset = self.offset;

        // Determine whether the delta looks like discrete ticks (small absolute
        // values ≤ 3) or high-resolution pixel deltas (larger values).
        let is_pixel_mode = total_dy.unsigned_abs() > 3;

        let line_delta = if is_pixel_mode {
            // Accumulate fractional sub-line offset.
            self.fractional += f64::from(total_dy) * self.config.pixel_scale;
            let whole = self.fractional.trunc() as isize;
            self.fractional -= whole as f64;
            whole
        } else {
            // Discrete ticks.
            self.fractional = 0.0;
            isize::from(total_dy as i16) * self.config.lines_per_tick as isize
        };

        // Apply delta. Positive dy = scroll up (increase offset, older lines).
        if line_delta > 0 {
            self.offset = self
                .offset
                .saturating_add(line_delta as usize)
                .min(max_offset);
        } else if line_delta < 0 {
            self.offset = self.offset.saturating_sub((-line_delta) as usize);
        }

        // Seed inertia velocity only when wheel input produced meaningful motion.
        if self.config.inertia_enabled {
            let moved = self.offset != previous_offset;
            if moved && line_delta != 0 {
                self.velocity = line_delta as f64;
                self.animating = true;
            } else {
                self.velocity = 0.0;
                self.animating = false;
            }
        }
    }

    /// Advance one frame of inertial scrolling. Call once per frame after
    /// `apply_wheel`. Returns `true` if the animation is still running
    /// (the renderer should schedule another frame).
    pub fn tick(&mut self, max_offset: usize) -> bool {
        if !self.animating {
            return false;
        }

        self.velocity *= self.config.inertia_friction;

        if self.velocity.abs() < self.config.inertia_stop_threshold {
            self.velocity = 0.0;
            self.fractional = 0.0;
            self.animating = false;
            return false;
        }

        // Apply inertial movement.
        self.fractional += self.velocity;
        let whole = self.fractional.trunc() as isize;
        self.fractional -= whole as f64;

        if whole > 0 {
            self.offset = self.offset.saturating_add(whole as usize).min(max_offset);
        } else if whole < 0 {
            self.offset = self.offset.saturating_sub((-whole) as usize);
        }

        // Stop if we hit the bounds.
        if self.offset == 0 || self.offset == max_offset {
            self.velocity = 0.0;
            self.fractional = 0.0;
            self.animating = false;
            return false;
        }

        true
    }

    /// Compute the viewport snapshot for the current frame.
    ///
    /// `total_scrollback_lines` is `Scrollback::len()` and `viewport_rows` is
    /// the terminal's visible row count.
    #[must_use]
    pub fn viewport(
        &mut self,
        total_scrollback_lines: usize,
        viewport_rows: usize,
    ) -> ViewportSnapshot {
        let viewport_len = viewport_rows.min(total_scrollback_lines);
        let max_offset = total_scrollback_lines.saturating_sub(viewport_len);

        // Clamp offset.
        self.offset = self.offset.min(max_offset);

        if viewport_len == 0 || total_scrollback_lines == 0 {
            return ViewportSnapshot {
                total_lines: total_scrollback_lines,
                viewport_start: total_scrollback_lines,
                viewport_end: total_scrollback_lines,
                render_start: total_scrollback_lines,
                render_end: total_scrollback_lines,
                sub_line_offset: 0.0,
                scroll_offset_from_bottom: self.offset,
                max_scroll_offset: max_offset,
                is_at_bottom: self.offset == 0,
                is_animating: self.animating,
            };
        }

        let newest_viewport_start = total_scrollback_lines.saturating_sub(viewport_len);
        let viewport_start = newest_viewport_start.saturating_sub(self.offset);
        let viewport_end = viewport_start.saturating_add(viewport_len);

        let overscan = self.config.overscan_lines;
        let render_start = viewport_start.saturating_sub(overscan);
        let render_end = viewport_end
            .saturating_add(overscan)
            .min(total_scrollback_lines);

        ViewportSnapshot {
            total_lines: total_scrollback_lines,
            viewport_start,
            viewport_end,
            render_start,
            render_end,
            sub_line_offset: self.fractional.fract().abs(),
            scroll_offset_from_bottom: self.offset,
            max_scroll_offset: max_offset,
            is_at_bottom: self.offset == 0,
            is_animating: self.animating,
        }
    }
}

// ---------------------------------------------------------------------------
// Frame stats for JSONL logging
// ---------------------------------------------------------------------------

/// Per-frame scroll metrics for JSONL event logs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollFrameStats {
    /// Current scroll offset from bottom.
    pub scroll_offset: usize,
    /// Total scrollback lines.
    pub total_scrollback: usize,
    /// Number of lines in the render range (viewport + overscan).
    pub render_lines: usize,
    /// Viewport height in terminal rows.
    pub viewport_rows: usize,
    /// Whether viewport is pinned to the bottom.
    pub at_bottom: bool,
    /// Whether inertia is active.
    pub animating: bool,
    /// Wheel events coalesced this frame.
    pub coalesced_events: u32,
}

impl ScrollFrameStats {
    /// Build stats from a viewport snapshot and coalescer output.
    #[must_use]
    pub fn from_snapshot(snap: &ViewportSnapshot, coalesced_events: u32) -> Self {
        Self {
            scroll_offset: snap.scroll_offset_from_bottom,
            total_scrollback: snap.total_lines,
            render_lines: snap.render_len(),
            viewport_rows: snap.viewport_len(),
            at_bottom: snap.is_at_bottom,
            animating: snap.is_animating,
            coalesced_events,
        }
    }
}

// ---------------------------------------------------------------------------
// Scrollback search core
// ---------------------------------------------------------------------------

/// Search tuning for scrollback text queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchConfig {
    /// Treat query and text as case-sensitive.
    pub case_sensitive: bool,
    /// Normalize both query and lines before matching.
    ///
    /// This uses Unicode NFKC and enables matching canonically equivalent text
    /// (`é` vs `e\u{301}`), plus compatibility forms.
    pub normalize_unicode: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            normalize_unicode: true,
        }
    }
}

/// One search hit in scrollback space.
///
/// `start_char`/`end_char` are character offsets in the normalized line text.
/// They are deterministic and stable for navigation. Highlight mapping back to
/// renderer cells is intentionally deferred to the overlay layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub line_idx: usize,
    pub start_char: usize,
    pub end_char: usize,
}

/// Immutable search result index with next/prev navigation helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchIndex {
    normalized_query: String,
    matches: Vec<SearchMatch>,
}

impl SearchIndex {
    /// Build a deterministic index of all non-overlapping matches.
    ///
    /// `lines` should be provided in scrollback order (`0 = oldest`).
    #[must_use]
    pub fn build<'a>(
        lines: impl IntoIterator<Item = &'a str>,
        query: &str,
        config: SearchConfig,
    ) -> Self {
        let normalized_query = normalize_for_search(query, config);
        if normalized_query.is_empty() {
            return Self {
                normalized_query,
                matches: Vec::new(),
            };
        }

        let mut matches = Vec::new();
        for (line_idx, line) in lines.into_iter().enumerate() {
            let normalized_line = normalize_for_search(line, config);
            if normalized_line.len() < normalized_query.len() || normalized_line.is_empty() {
                continue;
            }

            collect_line_matches(line_idx, &normalized_line, &normalized_query, &mut matches);
        }

        Self {
            normalized_query,
            matches,
        }
    }

    /// Build a deterministic index over physical lines with soft-wrap metadata.
    ///
    /// `lines` are provided in scrollback order (`0 = oldest`), where
    /// `wrapped = true` means this line is a soft-wrap continuation of the
    /// previous physical line.
    ///
    /// Search is performed on logical joined lines, and each match is reported
    /// against the first physical line index of that logical line.
    #[must_use]
    pub fn build_wrapped<'a>(
        lines: impl IntoIterator<Item = (&'a str, bool)>,
        query: &str,
        config: SearchConfig,
    ) -> Self {
        let normalized_query = normalize_for_search(query, config);
        if normalized_query.is_empty() {
            return Self {
                normalized_query,
                matches: Vec::new(),
            };
        }

        let mut matches = Vec::new();
        let mut logical_line = String::new();
        let mut logical_start_idx = 0usize;
        let mut has_logical = false;

        for (line_idx, (line, wrapped)) in lines.into_iter().enumerate() {
            let normalized = normalize_for_search(line, config);

            if !has_logical {
                logical_start_idx = line_idx;
                logical_line.clear();
                logical_line.push_str(&normalized);
                has_logical = true;
                continue;
            }

            if wrapped {
                logical_line.push_str(&normalized);
            } else {
                collect_line_matches(
                    logical_start_idx,
                    &logical_line,
                    &normalized_query,
                    &mut matches,
                );
                logical_start_idx = line_idx;
                logical_line.clear();
                logical_line.push_str(&normalized);
            }
        }

        if has_logical {
            collect_line_matches(
                logical_start_idx,
                &logical_line,
                &normalized_query,
                &mut matches,
            );
        }

        Self {
            normalized_query,
            matches,
        }
    }

    /// Normalized query used for matching.
    #[must_use]
    pub fn normalized_query(&self) -> &str {
        &self.normalized_query
    }

    /// Total match count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Whether no matches were found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// All matches in deterministic order.
    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    /// Index of the next match, wrapping at the end.
    #[must_use]
    pub fn next_index(&self, current: Option<usize>) -> Option<usize> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }
        Some(match current {
            Some(idx) => (idx + 1) % len,
            None => 0,
        })
    }

    /// Index of the previous match, wrapping at the beginning.
    #[must_use]
    pub fn prev_index(&self, current: Option<usize>) -> Option<usize> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }

        Some(match current {
            Some(0) | None => len - 1,
            Some(idx) => idx.saturating_sub(1).min(len - 1),
        })
    }
}

fn normalize_for_search(input: &str, config: SearchConfig) -> String {
    let normalized = if config.normalize_unicode {
        input.nfkc().collect::<String>()
    } else {
        input.to_owned()
    };

    if config.case_sensitive {
        normalized
    } else {
        normalized.to_lowercase()
    }
}

fn collect_line_matches(
    line_idx: usize,
    normalized_line: &str,
    normalized_query: &str,
    out: &mut Vec<SearchMatch>,
) {
    let mut char_starts: Vec<usize> = normalized_line.char_indices().map(|(idx, _)| idx).collect();
    char_starts.push(normalized_line.len());

    for (start_byte, _) in normalized_line.match_indices(normalized_query) {
        let end_byte = start_byte.saturating_add(normalized_query.len());
        out.push(SearchMatch {
            line_idx,
            start_char: byte_offset_to_char_index(&char_starts, start_byte),
            end_char: byte_offset_to_char_index(&char_starts, end_byte),
        });
    }
}

fn byte_offset_to_char_index(char_starts: &[usize], byte_offset: usize) -> usize {
    match char_starts.binary_search(&byte_offset) {
        Ok(idx) | Err(idx) => idx,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Modifiers, WheelInput};

    fn wheel(dy: i16) -> WheelInput {
        WheelInput {
            x: 0,
            y: 0,
            dx: 0,
            dy,
            mods: Modifiers::empty(),
        }
    }

    // -- ScrollConfig defaults --

    #[test]
    fn default_config_is_reasonable() {
        let cfg = ScrollConfig::default();
        assert!(cfg.overscan_lines > 0);
        assert!(cfg.lines_per_tick > 0);
        assert!(cfg.inertia_friction > 0.0 && cfg.inertia_friction < 1.0);
    }

    // -- WheelCoalescer --

    #[test]
    fn coalescer_starts_empty() {
        let c = WheelCoalescer::new();
        assert!(!c.has_events());
    }

    #[test]
    fn coalescer_accumulates_deltas() {
        let mut c = WheelCoalescer::new();
        c.push(&wheel(1));
        c.push(&wheel(2));
        c.push(&wheel(-1));
        assert!(c.has_events());

        let (dy, count) = c.drain();
        assert_eq!(dy, 2); // 1 + 2 + (-1)
        assert_eq!(count, 3);
        assert!(!c.has_events());
    }

    #[test]
    fn coalescer_drain_resets() {
        let mut c = WheelCoalescer::new();
        c.push(&wheel(5));
        let _ = c.drain();
        let (dy, count) = c.drain();
        assert_eq!(dy, 0);
        assert_eq!(count, 0);
    }

    #[test]
    fn coalescer_saturates_positive_overflow() {
        let mut c = WheelCoalescer::new();
        c.accumulated_dy = i32::MAX - 1;
        c.event_count = u32::MAX;
        c.push(&wheel(10));

        let (dy, count) = c.drain();
        assert_eq!(dy, i32::MAX);
        assert_eq!(count, u32::MAX);
    }

    #[test]
    fn coalescer_saturates_negative_overflow() {
        let mut c = WheelCoalescer::new();
        c.accumulated_dy = i32::MIN + 1;
        c.event_count = u32::MAX - 1;
        c.push(&wheel(-10));
        c.push(&wheel(-10));

        let (dy, count) = c.drain();
        assert_eq!(dy, i32::MIN);
        assert_eq!(count, u32::MAX);
    }

    // -- ScrollState basic --

    #[test]
    fn initial_state_is_at_bottom() {
        let state = ScrollState::with_defaults();
        assert_eq!(state.offset(), 0);
        assert!(!state.is_animating());
    }

    #[test]
    fn snap_to_bottom_clears_offset() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(42);
        assert_eq!(state.offset(), 42);
        state.snap_to_bottom();
        assert_eq!(state.offset(), 0);
        assert!(!state.is_animating());
    }

    #[test]
    fn snap_to_top() {
        let mut state = ScrollState::with_defaults();
        state.snap_to_top(100, 24);
        assert_eq!(state.offset(), 76);
    }

    #[test]
    fn jump_to_line_in_empty_buffer_is_noop() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(42);
        state.jump_to_line(0, 24, 0);
        assert_eq!(state.offset(), 0);
        assert!(!state.is_animating());
    }

    #[test]
    fn jump_to_line_near_bottom_keeps_bottom() {
        let mut state = ScrollState::with_defaults();
        state.jump_to_line(100, 24, 99);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn jump_to_line_places_target_inside_viewport() {
        let mut state = ScrollState::with_defaults();
        state.jump_to_line(100, 24, 40);
        let snap = state.viewport(100, 24);
        assert!(snap.viewport_start <= 40);
        assert!(40 < snap.viewport_end);
    }

    #[test]
    fn jump_to_line_near_top_clamps_to_oldest_window() {
        let mut state = ScrollState::with_defaults();
        state.jump_to_line(100, 24, 0);
        let snap = state.viewport(100, 24);
        assert_eq!(snap.viewport_start, 0);
        assert_eq!(snap.viewport_end, 24);
    }

    #[test]
    fn scroll_lines_positive() {
        let mut state = ScrollState::with_defaults();
        state.scroll_lines(10);
        assert_eq!(state.offset(), 10);
    }

    #[test]
    fn scroll_lines_negative_clamps_at_zero() {
        let mut state = ScrollState::with_defaults();
        state.scroll_lines(-5);
        assert_eq!(state.offset(), 0);
    }

    // -- apply_wheel --

    #[test]
    fn discrete_wheel_scrolls_by_lines_per_tick() {
        let mut state = ScrollState::with_defaults();
        let lpt = state.config.lines_per_tick;
        state.apply_wheel(1, 100);
        assert_eq!(state.offset(), lpt);
    }

    #[test]
    fn discrete_wheel_negative_scrolls_down() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(20);
        state.apply_wheel(-1, 100);
        let expected = 20 - state.config.lines_per_tick;
        assert_eq!(state.offset(), expected);
    }

    #[test]
    fn wheel_zero_is_noop() {
        let mut state = ScrollState::with_defaults();
        state.apply_wheel(0, 100);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn wheel_clamps_at_max_offset() {
        let mut state = ScrollState::with_defaults();
        state.apply_wheel(1, 2);
        assert_eq!(state.offset(), 2); // clamped to max
    }

    #[test]
    fn wheel_clamps_at_zero() {
        let mut state = ScrollState::with_defaults();
        state.apply_wheel(-1, 100);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn wheel_fractional_noop_does_not_start_inertia() {
        let mut state = ScrollState::with_defaults();
        // Pixel-mode path with a fractional delta smaller than one full line.
        state.apply_wheel(4, 100);
        assert!(!state.is_animating());
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn wheel_clamped_movement_does_not_start_inertia() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(5);
        // At max offset already, positive scroll cannot move further.
        state.apply_wheel(1, 5);
        assert!(!state.is_animating());
        assert_eq!(state.offset(), 5);
    }

    // -- Inertia --

    #[test]
    fn inertia_decays_to_stop() {
        let mut state = ScrollState::with_defaults();
        state.apply_wheel(3, 10000);
        let initial = state.offset();
        assert!(state.is_animating());

        // Tick until animation stops with an explicit deterministic upper bound.
        let mut stopped = false;
        for _ in 0..=500 {
            if !state.tick(10000) {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "inertia did not stop within 500 ticks");

        assert!(!state.is_animating());
        assert!(
            state.offset() > initial,
            "inertia should have scrolled further"
        );
    }

    #[test]
    fn inertia_disabled_stops_immediately() {
        let config = ScrollConfig {
            inertia_enabled: false,
            ..ScrollConfig::default()
        };
        let mut state = ScrollState::new(config);
        state.apply_wheel(2, 100);
        assert!(!state.is_animating());
        assert!(!state.tick(100));
    }

    #[test]
    fn inertia_stops_at_boundary() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(1);
        state.velocity = -10.0;
        state.animating = true;
        let still_going = state.tick(100);
        // Should stop because offset reached 0.
        assert!(!still_going);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn disabling_inertia_while_animating_stops_immediately() {
        let mut state = ScrollState::with_defaults();
        state.apply_wheel(3, 10_000);
        assert!(state.is_animating());

        let cfg = ScrollConfig {
            inertia_enabled: false,
            ..ScrollConfig::default()
        };
        state.set_config(cfg);

        assert!(!state.is_animating());
        assert!(!state.tick(10_000));
    }

    #[test]
    fn changing_config_with_inertia_enabled_keeps_animation() {
        let mut state = ScrollState::with_defaults();
        state.apply_wheel(3, 10_000);
        assert!(state.is_animating());

        let cfg = ScrollConfig {
            inertia_friction: 0.90,
            ..ScrollConfig::default()
        };
        state.set_config(cfg);

        assert!(state.is_animating());
    }

    // -- Viewport --

    #[test]
    fn viewport_at_bottom() {
        let mut state = ScrollState::with_defaults();
        let snap = state.viewport(100, 24);
        assert!(snap.is_at_bottom);
        assert_eq!(snap.viewport_start, 76);
        assert_eq!(snap.viewport_end, 100);
        assert_eq!(snap.viewport_len(), 24);
        assert_eq!(snap.scroll_offset_from_bottom, 0);
        assert_eq!(snap.max_scroll_offset, 76);
    }

    #[test]
    fn viewport_scrolled_up() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(10);
        let snap = state.viewport(100, 24);
        assert!(!snap.is_at_bottom);
        assert_eq!(snap.viewport_start, 66);
        assert_eq!(snap.viewport_end, 90);
    }

    #[test]
    fn viewport_overscan() {
        let config = ScrollConfig {
            overscan_lines: 5,
            ..ScrollConfig::default()
        };
        let mut state = ScrollState::new(config);
        state.set_offset(10);
        let snap = state.viewport(100, 24);
        assert_eq!(snap.render_start, 61); // viewport_start(66) - 5
        assert_eq!(snap.render_end, 95); // viewport_end(90) + 5
        assert!(snap.render_len() > snap.viewport_len());
    }

    #[test]
    fn viewport_overscan_clamped_at_boundaries() {
        let config = ScrollConfig {
            overscan_lines: 10,
            ..ScrollConfig::default()
        };
        let mut state = ScrollState::new(config);
        // Near the top.
        state.set_offset(95);
        let snap = state.viewport(100, 24);
        assert_eq!(snap.render_start, 0); // clamped at 0
        assert!(snap.render_end <= 100);
    }

    #[test]
    fn viewport_small_scrollback() {
        let mut state = ScrollState::with_defaults();
        let snap = state.viewport(5, 24);
        // Only 5 lines of scrollback, viewport wants 24.
        assert_eq!(snap.viewport_start, 0);
        assert_eq!(snap.viewport_end, 5);
        assert_eq!(snap.viewport_len(), 5);
        assert!(snap.is_at_bottom);
    }

    #[test]
    fn viewport_empty_scrollback() {
        let mut state = ScrollState::with_defaults();
        let snap = state.viewport(0, 24);
        assert_eq!(snap.viewport_len(), 0);
        assert_eq!(snap.render_len(), 0);
        assert!(snap.is_at_bottom);
    }

    #[test]
    fn viewport_zero_rows_with_nonempty_scrollback() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(10);
        let snap = state.viewport(100, 0);
        assert_eq!(snap.viewport_start, 100);
        assert_eq!(snap.viewport_end, 100);
        assert_eq!(snap.render_start, 100);
        assert_eq!(snap.render_end, 100);
        assert_eq!(snap.viewport_len(), 0);
        assert_eq!(snap.max_scroll_offset, 100);
        assert_eq!(snap.scroll_offset_from_bottom, 10);
        assert!(!snap.is_at_bottom);
    }

    #[test]
    fn viewport_clamps_excess_offset() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(999);
        let snap = state.viewport(50, 24);
        assert_eq!(snap.scroll_offset_from_bottom, 26); // clamped to max
        assert_eq!(snap.viewport_start, 0);
    }

    // -- ScrollFrameStats --

    #[test]
    fn frame_stats_from_snapshot() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(10);
        let snap = state.viewport(100, 24);
        let stats = ScrollFrameStats::from_snapshot(&snap, 3);
        assert_eq!(stats.scroll_offset, 10);
        assert_eq!(stats.total_scrollback, 100);
        assert_eq!(stats.viewport_rows, 24);
        assert!(!stats.at_bottom);
        assert_eq!(stats.coalesced_events, 3);
    }

    // -- Pixel-mode wheel (high-resolution trackpad) --

    #[test]
    fn pixel_mode_wheel_accumulates_fractional() {
        let mut state = ScrollState::with_defaults();
        // dy > 3 triggers pixel mode. With default pixel_scale = 1/40,
        // need ~40 px of delta to scroll 1 line.
        state.apply_wheel(20, 1000); // 20 * (1/40) = 0.5 lines, rounds to 0
        assert_eq!(state.offset(), 0);

        state.apply_wheel(20, 1000); // accumulated 1.0 line
        assert_eq!(state.offset(), 1);
    }

    #[test]
    fn pixel_mode_large_delta() {
        let mut state = ScrollState::with_defaults();
        // 120 px * (1/40) = 3.0 lines
        state.apply_wheel(120, 1000);
        assert_eq!(state.offset(), 3);
    }

    #[test]
    fn pixel_mode_negative_delta_accumulates_before_scrolling() {
        let mut state = ScrollState::with_defaults();
        state.set_offset(10);

        // First half-line worth of negative pixel delta: no full-line movement.
        state.apply_wheel(-20, 1000); // -20 * (1/40) = -0.5
        assert_eq!(state.offset(), 10);
        assert!(!state.is_animating());

        // Second half-line crosses -1.0 total and scrolls one line toward newer output.
        state.apply_wheel(-20, 1000);
        assert_eq!(state.offset(), 9);
        assert!(state.is_animating());
    }

    // -- Integration: coalescer + state --

    #[test]
    fn coalescer_feeds_state() {
        let mut coalescer = WheelCoalescer::new();
        let mut state = ScrollState::with_defaults();

        coalescer.push(&wheel(1));
        coalescer.push(&wheel(1));
        let (dy, _count) = coalescer.drain();

        state.apply_wheel(dy, 100);
        // 2 discrete ticks * lines_per_tick(3) = 6
        assert_eq!(state.offset(), 6);
    }

    // -- Search core --

    #[test]
    fn search_empty_query_has_no_hits() {
        let lines = ["alpha", "beta"];
        let idx = SearchIndex::build(lines.iter().copied(), "", SearchConfig::default());
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
        assert_eq!(idx.next_index(None), None);
        assert_eq!(idx.prev_index(None), None);
    }

    #[test]
    fn search_case_insensitive_by_default() {
        let lines = ["Alpha beta", "gamma"];
        let idx = SearchIndex::build(lines.iter().copied(), "ALPHA", SearchConfig::default());
        assert_eq!(idx.len(), 1);
        let hit = idx.matches()[0];
        assert_eq!(hit.line_idx, 0);
        assert_eq!((hit.start_char, hit.end_char), (0, 5));
    }

    #[test]
    fn search_unicode_normalization_matches_composed_and_decomposed() {
        let lines = ["Cafe\u{301} noir", "other"];
        let idx = SearchIndex::build(lines.iter().copied(), "Café", SearchConfig::default());
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.matches()[0].line_idx, 0);

        let strict = SearchIndex::build(
            lines.iter().copied(),
            "Café",
            SearchConfig {
                normalize_unicode: false,
                ..SearchConfig::default()
            },
        );
        assert!(strict.is_empty());
    }

    #[test]
    fn search_next_prev_wraps() {
        let lines = ["abc abc", "abc"];
        let idx = SearchIndex::build(lines.iter().copied(), "abc", SearchConfig::default());
        assert_eq!(idx.len(), 3);

        let i0 = idx.next_index(None).expect("first");
        let i1 = idx.next_index(Some(i0)).expect("second");
        let i2 = idx.next_index(Some(i1)).expect("third");
        let i3 = idx.next_index(Some(i2)).expect("wrapped");
        assert_eq!((i0, i1, i2, i3), (0, 1, 2, 0));

        let p2 = idx.prev_index(None).expect("last");
        let p1 = idx.prev_index(Some(p2)).expect("prev");
        let p0 = idx.prev_index(Some(p1)).expect("prev");
        let pw = idx.prev_index(Some(p0)).expect("wrapped");
        assert_eq!((p2, p1, p0, pw), (2, 1, 0, 2));
    }

    #[test]
    fn search_build_wrapped_matches_across_soft_wrap_boundary() {
        let lines = [("hel", false), ("lo world", true), ("next", false)];
        let idx =
            SearchIndex::build_wrapped(lines.iter().copied(), "hello", SearchConfig::default());
        assert_eq!(idx.len(), 1);
        let hit = idx.matches()[0];
        assert_eq!(hit.line_idx, 0);
        assert_eq!((hit.start_char, hit.end_char), (0, 5));
    }

    #[test]
    fn search_build_wrapped_does_not_cross_hard_line_breaks() {
        let lines = [("foo", false), ("bar", true), ("baz", false)];

        let joined =
            SearchIndex::build_wrapped(lines.iter().copied(), "foobar", SearchConfig::default());
        assert_eq!(joined.len(), 1);
        assert_eq!(joined.matches()[0].line_idx, 0);

        let hard_break =
            SearchIndex::build_wrapped(lines.iter().copied(), "barbaz", SearchConfig::default());
        assert!(hard_break.is_empty());
    }

    #[test]
    fn search_navigation_handles_out_of_range_current_index() {
        let lines = [("alpha beta", false), ("beta", false)];
        let idx =
            SearchIndex::build_wrapped(lines.iter().copied(), "beta", SearchConfig::default());
        assert_eq!(idx.len(), 2);

        // next_index wraps by modulo, so very large index resolves deterministically.
        assert_eq!(idx.next_index(Some(999)), Some(0));
        // prev_index clamps out-of-range indexes to the final valid match.
        assert_eq!(idx.prev_index(Some(999)), Some(1));
    }

    #[test]
    fn stress_100k_scrollback_search_index_is_bounded() {
        let mut lines = Vec::with_capacity(100_000);
        for i in 0..100_000 {
            if i % 1_000 == 0 {
                lines.push(format!("[{i}] error: unicode Café path"));
            } else {
                lines.push(format!("line {i}: all systems nominal"));
            }
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();

        let started = std::time::Instant::now();
        let idx = SearchIndex::build(refs.iter().copied(), "cafe\u{301}", SearchConfig::default());
        let elapsed = started.elapsed();

        assert_eq!(idx.len(), 100);
        assert!(elapsed < std::time::Duration::from_secs(5));
    }

    // -- 100k-line stress test --

    /// Verify that viewport computation with 100k lines of scrollback is fast
    /// and produces correct results at various scroll positions.
    #[test]
    fn stress_100k_scrollback_viewport_sweep() {
        let scrollback = 100_000usize;
        let rows = 24usize;
        let mut state = ScrollState::with_defaults();

        // Sweep scroll positions from bottom to top in large steps.
        let positions = [0, 100, 1_000, 10_000, 50_000, 99_976];
        for &pos in &positions {
            state.set_offset(pos);
            let snap = state.viewport(scrollback, rows);

            // Basic invariants.
            assert_eq!(snap.total_lines, scrollback);
            assert_eq!(snap.viewport_len(), rows);
            assert!(snap.render_len() >= snap.viewport_len());
            assert!(snap.render_start <= snap.viewport_start);
            assert!(snap.viewport_end <= snap.render_end);
            assert!(snap.render_end <= scrollback);
        }
    }

    /// Simulate 1000 frames of continuous scrolling through 100k-line scrollback
    /// to verify correctness under sustained scroll.
    #[test]
    fn stress_100k_continuous_scroll_1000_frames() {
        let scrollback = 100_000usize;
        let rows = 24usize;
        let max_off = scrollback.saturating_sub(rows);
        let mut state = ScrollState::with_defaults();

        for frame in 0..1000 {
            // Alternate scrolling up and down.
            let dy = if frame < 500 { 1 } else { -1 };
            state.apply_wheel(dy, max_off);
            state.tick(max_off);

            let snap = state.viewport(scrollback, rows);
            assert_eq!(snap.viewport_len(), rows);
            assert!(snap.render_end <= scrollback);
        }

        // After scrolling up 500 then down 500, should be roughly near bottom.
        let snap = state.viewport(scrollback, rows);
        // Inertia may overshoot, but offset should be reasonable.
        assert!(snap.scroll_offset_from_bottom < 100);
    }

    /// Verify render_len stays bounded with overscan on 100k scrollback.
    #[test]
    fn stress_100k_overscan_bounded() {
        let config = ScrollConfig {
            overscan_lines: 10,
            ..ScrollConfig::default()
        };
        let mut state = ScrollState::new(config);
        let scrollback = 100_000usize;
        let rows = 24usize;

        // Mid-scroll position.
        state.set_offset(50_000);
        let snap = state.viewport(scrollback, rows);

        // Render range should be viewport + 2*overscan = 24 + 20 = 44.
        assert_eq!(snap.render_len(), rows + 20);
    }
}
