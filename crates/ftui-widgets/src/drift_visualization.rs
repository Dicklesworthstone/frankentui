#![forbid(unsafe_code)]

//! Drift-triggered fallback visualization widget (bd-1lgz8.2).
//!
//! Renders live posterior sparklines per decision domain with color-coded
//! confidence zones, fallback trigger indicators, and regime transition
//! banners. Designed for the galaxy-brain transparency demo.
//!
//! # Components
//!
//! - **`DriftSnapshot`**: A single frame's worth of per-domain confidence data.
//! - **`DriftTimeline`**: Ring buffer of recent snapshots for sparkline rendering.
//! - **`DriftVisualization`**: Compound widget rendering domain sparklines with
//!   fallback indicators and regime banners.

use crate::borders::{BorderSet, BorderType};
use crate::sparkline::Sparkline;
use crate::{Widget, apply_style, clear_text_area, draw_text_span};
use ftui_a11y::node::{A11yNodeInfo, A11yRole, LiveRegion};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::transparency::TrafficLight;
use ftui_runtime::unified_evidence::DecisionDomain;
use ftui_style::Style;

// ---------------------------------------------------------------------------
// Color palette
// ---------------------------------------------------------------------------

const ZONE_GREEN: PackedRgba = PackedRgba::rgb(0, 180, 0);
const ZONE_YELLOW: PackedRgba = PackedRgba::rgb(200, 180, 0);
const ZONE_RED: PackedRgba = PackedRgba::rgb(200, 50, 50);
const FALLBACK_FG: PackedRgba = PackedRgba::rgb(255, 80, 80);
const FALLBACK_BG: PackedRgba = PackedRgba::rgb(80, 10, 10);
const REGIME_FG: PackedRgba = PackedRgba::rgb(255, 200, 100);
const DIM_FG: PackedRgba = PackedRgba::rgb(120, 120, 120);
const LABEL_FG: PackedRgba = PackedRgba::rgb(160, 180, 200);

// ---------------------------------------------------------------------------
// DriftSnapshot — one frame's per-domain data
// ---------------------------------------------------------------------------

/// A single frame's confidence snapshot for one decision domain.
#[derive(Debug, Clone, Copy)]
pub struct DomainSnapshot {
    /// The decision domain.
    pub domain: DecisionDomain,
    /// Confidence value (0.0 = no confidence, 1.0 = full confidence).
    pub confidence: f64,
    /// Current traffic light signal.
    pub signal: TrafficLight,
    /// Whether the domain is currently in fallback mode.
    pub in_fallback: bool,
    /// Active strategy/action label index (for regime display).
    pub regime_label: &'static str,
}

/// Snapshot of all domains at a single point in time.
#[derive(Debug, Clone)]
pub struct DriftSnapshot {
    /// Per-domain snapshots.
    pub domains: Vec<DomainSnapshot>,
    /// Frame number or tick count for timeline reference.
    pub frame_id: u64,
}

// ---------------------------------------------------------------------------
// DriftTimeline — ring buffer of snapshots
// ---------------------------------------------------------------------------

/// Ring buffer of recent drift snapshots for sparkline rendering.
#[derive(Debug, Clone)]
pub struct DriftTimeline {
    /// Circular buffer of snapshots.
    snapshots: Vec<DriftSnapshot>,
    /// Write cursor (next position to write).
    write_pos: usize,
    /// Number of snapshots stored (≤ capacity).
    len: usize,
    /// Maximum capacity.
    capacity: usize,
}

impl DriftTimeline {
    /// Create a new timeline with the given capacity (max frames to retain).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            snapshots: Vec::with_capacity(capacity),
            write_pos: 0,
            len: 0,
            capacity,
        }
    }

    /// Push a new snapshot into the timeline.
    pub fn push(&mut self, snapshot: DriftSnapshot) {
        if self.snapshots.len() < self.capacity {
            self.snapshots.push(snapshot);
        } else {
            self.snapshots[self.write_pos] = snapshot;
        }
        self.write_pos = (self.write_pos + 1) % self.capacity;
        self.len = (self.len + 1).min(self.capacity);
    }

    /// Number of snapshots stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the timeline is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterate snapshots in chronological order (oldest first).
    pub fn iter_chronological(&self) -> impl Iterator<Item = &DriftSnapshot> {
        let start = if self.len < self.capacity {
            0
        } else {
            self.write_pos
        };

        (0..self.len).map(move |i| {
            let idx = (start + i) % self.capacity;
            &self.snapshots[idx]
        })
    }

    /// Extract confidence values for a specific domain in chronological order.
    pub fn confidence_series(&self, domain: DecisionDomain) -> Vec<f64> {
        self.iter_chronological()
            .map(|snap| {
                snap.domains
                    .iter()
                    .find(|d| d.domain == domain)
                    .map_or(0.0, |d| d.confidence)
            })
            .collect()
    }

    /// Find the most recent snapshot where a domain transitioned into fallback.
    pub fn last_fallback_trigger(&self, domain: DecisionDomain) -> Option<usize> {
        let series: Vec<bool> = self
            .iter_chronological()
            .map(|snap| {
                snap.domains
                    .iter()
                    .find(|d| d.domain == domain)
                    .is_some_and(|d| d.in_fallback)
            })
            .collect();

        if series.first().copied().unwrap_or(false) {
            return Some(0);
        }

        // Find the last rising edge (false -> true)
        (1..series.len())
            .rev()
            .find(|&i| series[i] && !series[i - 1])
    }

    /// Get the latest snapshot, if any.
    #[must_use]
    pub fn latest(&self) -> Option<&DriftSnapshot> {
        if self.len == 0 {
            return None;
        }
        let idx = if self.write_pos == 0 {
            self.capacity - 1
        } else {
            self.write_pos - 1
        };
        self.snapshots.get(idx)
    }
}

// ---------------------------------------------------------------------------
// DriftVisualization widget
// ---------------------------------------------------------------------------

/// Compound widget rendering drift-triggered fallback visualization.
///
/// Shows one sparkline row per decision domain, color-coded by confidence zone:
/// - Green zone (>0.7): high confidence, Bayesian strategy active
/// - Yellow zone (0.3–0.7): moderate confidence, potential drift
/// - Red zone (<0.3): low confidence, fallback likely/active
///
/// When a domain enters fallback, a vertical marker appears on the sparkline
/// and a regime banner flashes.
#[derive(Debug, Clone)]
pub struct DriftVisualization<'a> {
    /// The timeline data source.
    timeline: &'a DriftTimeline,
    /// Which domains to display (None = all from latest snapshot).
    domains: Option<Vec<DecisionDomain>>,
    /// Border type for the widget.
    border_type: BorderType,
    /// Base style.
    style: Style,
    /// Whether to show the regime banner.
    show_regime_banner: bool,
    /// Fallback threshold (confidence below this = red zone).
    fallback_threshold: f64,
    /// Caution threshold (confidence below this = yellow zone).
    caution_threshold: f64,
}

impl<'a> DriftVisualization<'a> {
    /// Create a new drift visualization from a timeline.
    #[must_use]
    pub fn new(timeline: &'a DriftTimeline) -> Self {
        Self {
            timeline,
            domains: None,
            border_type: BorderType::Rounded,
            style: Style::default(),
            show_regime_banner: true,
            fallback_threshold: 0.3,
            caution_threshold: 0.7,
        }
    }

    /// Only display the specified domains.
    #[must_use]
    pub fn domains(mut self, domains: Vec<DecisionDomain>) -> Self {
        self.domains = Some(domains);
        self
    }

    /// Set the border type.
    #[must_use]
    pub fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    /// Set the base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Enable or disable the regime banner row.
    #[must_use]
    pub fn show_regime_banner(mut self, show: bool) -> Self {
        self.show_regime_banner = show;
        self
    }

    /// Set the fallback confidence threshold (default 0.3).
    #[must_use]
    pub fn fallback_threshold(mut self, t: f64) -> Self {
        self.fallback_threshold = t;
        self
    }

    /// Set the caution confidence threshold (default 0.7).
    #[must_use]
    pub fn caution_threshold(mut self, t: f64) -> Self {
        self.caution_threshold = t;
        self
    }

    /// Determine which domains to render.
    fn active_domains(&self) -> Vec<DecisionDomain> {
        if let Some(ref domains) = self.domains {
            return domains.clone();
        }
        if let Some(latest) = self.timeline.latest() {
            latest.domains.iter().map(|d| d.domain).collect()
        } else {
            Vec::new()
        }
    }

    /// Color for a confidence value.
    fn confidence_color(&self, confidence: f64) -> PackedRgba {
        let fallback = self.fallback_threshold.clamp(0.0, 1.0);
        let caution = self.caution_threshold.clamp(fallback, 1.0);

        if confidence >= caution {
            ZONE_GREEN
        } else if caution > fallback && confidence >= fallback {
            // Interpolate yellow
            let t = (confidence - fallback) / (caution - fallback);
            lerp_color(ZONE_YELLOW, ZONE_GREEN, t)
        } else {
            // Interpolate red
            let t = if fallback <= f64::EPSILON {
                0.0
            } else {
                confidence / fallback
            };
            lerp_color(ZONE_RED, ZONE_YELLOW, t)
        }
    }

    /// Minimum height needed for the widget.
    #[must_use]
    pub fn min_height(&self) -> u16 {
        let domains = self.active_domains();
        let domain_rows = domains.len() as u16;
        // border_top + title + domains*(label_row + sparkline_row) + banner? + border_bottom
        let mut h: u16 = 2; // top + bottom border
        h += 1; // title row
        h += domain_rows * 2; // label + sparkline per domain
        if self.show_regime_banner {
            h += 1;
        }
        h
    }

    fn render_domain_row(
        &self,
        domain: DecisionDomain,
        x: u16,
        y: u16,
        width: u16,
        frame: &mut Frame,
    ) -> u16 {
        let deg = frame.buffer.degradation;
        let apply_styling = deg.apply_styling();
        let max_x = x + width;

        // Row 1: Domain label + current confidence badge
        let label = domain.as_str();
        let label_style = if apply_styling {
            Style::new().fg(LABEL_FG)
        } else {
            Style::default()
        };
        let mut cx = draw_text_span(frame, x, y, label, label_style, max_x);

        // Current confidence badge
        if let Some(latest) = self.timeline.latest()
            && let Some(ds) = latest.domains.iter().find(|d| d.domain == domain)
        {
            let conf_pct = format!(" {:.0}%", ds.confidence * 100.0);
            let conf_color = self.confidence_color(ds.confidence);
            let conf_style = if apply_styling {
                Style::new().fg(conf_color).bold()
            } else {
                Style::default()
            };
            if cx < max_x {
                cx = draw_text_span(frame, cx, y, " ", Style::default(), max_x);
            }
            cx = draw_text_span(frame, cx, y, &conf_pct, conf_style, max_x);

            if ds.in_fallback {
                let fb_style = if apply_styling {
                    Style::new().fg(FALLBACK_FG).bg(FALLBACK_BG).bold()
                } else {
                    Style::default()
                };
                if cx < max_x {
                    cx = draw_text_span(frame, cx, y, " ", Style::default(), max_x);
                }
                cx = draw_text_span(frame, cx, y, " FALLBACK ", fb_style, max_x);
            }
            let _ = cx;
        }

        // Row 2: Sparkline
        let series = self.timeline.confidence_series(domain);
        if !series.is_empty() {
            let sparkline_width = width.min(series.len() as u16);
            // Take the last `sparkline_width` values
            let start = series.len().saturating_sub(sparkline_width as usize);
            let visible = &series[start..];

            let sparkline = Sparkline::new(visible)
                .bounds(0.0, 1.0)
                .gradient(ZONE_RED, ZONE_GREEN);
            let spark_area = Rect::new(x, y + 1, sparkline_width, 1);
            sparkline.render(spark_area, frame);

            // Overlay fallback trigger marker (vertical bar at trigger point)
            if let Some(trigger_idx) = self.timeline.last_fallback_trigger(domain) {
                let visible_start = series.len().saturating_sub(sparkline_width as usize);
                if trigger_idx >= visible_start {
                    let marker_x = x + (trigger_idx - visible_start) as u16;
                    if marker_x < max_x {
                        let mut cell = Cell::from_char('|');
                        if apply_styling {
                            apply_style(&mut cell, Style::new().fg(FALLBACK_FG).bold());
                        }
                        frame.buffer.set_fast(marker_x, y + 1, cell);
                    }
                }
            }
        }

        y + 2 // consumed 2 rows
    }

    fn render_regime_banner(&self, x: u16, y: u16, max_x: u16, frame: &mut Frame) {
        let Some(latest) = self.timeline.latest() else {
            return;
        };
        let apply_styling = frame.buffer.degradation.apply_styling();

        // Find any domain in fallback
        let fallback_domain = latest.domains.iter().find(|d| d.in_fallback);

        if let Some(ds) = fallback_domain {
            let banner = format!(
                " REGIME: {} -> deterministic ({}) ",
                ds.domain.as_str(),
                ds.regime_label,
            );
            let style = if apply_styling {
                Style::new().fg(REGIME_FG).bg(FALLBACK_BG).bold()
            } else {
                Style::default()
            };
            draw_text_span(frame, x, y, &banner, style, max_x);
        } else {
            // Normal operation
            let style = if apply_styling {
                Style::new().fg(DIM_FG)
            } else {
                Style::default()
            };
            draw_text_span(frame, x, y, "All domains: Bayesian (normal)", style, max_x);
        }
    }
}

impl Widget for DriftVisualization<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        if area.width < 6 || area.height < 4 {
            clear_text_area(frame, area, Style::default());
            return;
        }

        let deg = frame.buffer.degradation;
        if !deg.render_content() {
            clear_text_area(frame, area, Style::default());
            return;
        }

        let base_style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        clear_text_area(frame, area, base_style);

        // Draw border
        if deg.render_decorative() {
            let set = if deg.use_unicode_borders() {
                self.border_type.to_border_set()
            } else {
                BorderSet::ASCII
            };
            let border_style = if deg.apply_styling() {
                Style::new().fg(LABEL_FG)
            } else {
                Style::default()
            };
            render_border(area, frame, set, border_style);
        }

        // Inner area
        let inner_x = area.x.saturating_add(1);
        let inner_max_x = area.right().saturating_sub(1);
        let inner_width = inner_max_x.saturating_sub(inner_x);
        let mut y = area.y.saturating_add(1);
        let max_y = area.bottom().saturating_sub(1);

        if inner_width < 4 || y >= max_y {
            return;
        }

        // Title row
        let title_style = if deg.apply_styling() {
            Style::new().fg(LABEL_FG).bold()
        } else {
            Style::default()
        };
        draw_text_span(frame, inner_x, y, "Drift Monitor", title_style, inner_max_x);
        y += 1;

        // Domain rows (scoped under a Drift Monitor accessibility Group when enabled)
        let domains = self.active_domains();
        let a11y_bounds = frame.buffer.current_scissor().intersection(&area);
        if frame.a11y_enabled() && !a11y_bounds.is_empty() {
            let latest = self.timeline.latest();
            let fallback = latest.and_then(|snap| snap.domains.iter().find(|d| d.in_fallback));
            let domain_word = if domains.len() == 1 {
                "domain"
            } else {
                "domains"
            };
            let desc = if let Some(ds) = fallback {
                format!(
                    "{} {domain_word}; fallback active on {} ({})",
                    domains.len(),
                    ds.domain.as_str(),
                    ds.regime_label
                )
            } else if latest.is_none() {
                format!("{} {domain_word}; no snapshots", domains.len())
            } else {
                format!("{} {domain_word}; all Bayesian (normal)", domains.len())
            };
            let mut container = A11yNodeInfo::new(
                A11yNodeInfo::stable_id_for(A11yRole::Group, area),
                A11yRole::Group,
                a11y_bounds,
            )
            .with_name("Drift Monitor")
            .with_description(desc);
            if fallback.is_some() {
                container = container.with_live_region(LiveRegion::Polite);
            }
            frame.with_a11y_scope(container, |frame| {
                for domain in &domains {
                    if y + 1 >= max_y {
                        break;
                    }
                    y = self.render_domain_row(*domain, inner_x, y, inner_width, frame);
                }
                if self.show_regime_banner && y < max_y {
                    self.render_regime_banner(inner_x, y, inner_max_x, frame);
                }
            });
            return;
        }

        for domain in &domains {
            if y + 1 >= max_y {
                break;
            }
            y = self.render_domain_row(*domain, inner_x, y, inner_width, frame);
        }

        // Regime banner
        if self.show_regime_banner && y < max_y {
            self.render_regime_banner(inner_x, y, inner_max_x, frame);
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0) as f32;
    let r = (a.r() as f32 * (1.0 - t) + b.r() as f32 * t).round() as u8;
    let g = (a.g() as f32 * (1.0 - t) + b.g() as f32 * t).round() as u8;
    let b_val = (a.b() as f32 * (1.0 - t) + b.b() as f32 * t).round() as u8;
    PackedRgba::rgb(r, g, b_val)
}

fn render_border(area: Rect, frame: &mut Frame, set: BorderSet, style: Style) {
    let border_cell = |c: char| -> Cell {
        let mut cell = Cell::from_char(c);
        apply_style(&mut cell, style);
        cell
    };

    let right_x = area.right().saturating_sub(1);
    let bottom_y = area.bottom().saturating_sub(1);

    // Edges
    for x in area.x..area.right() {
        frame
            .buffer
            .set_fast(x, area.y, border_cell(set.horizontal));
        frame
            .buffer
            .set_fast(x, bottom_y, border_cell(set.horizontal));
    }
    for y in area.y..area.bottom() {
        frame.buffer.set_fast(area.x, y, border_cell(set.vertical));
        frame.buffer.set_fast(right_x, y, border_cell(set.vertical));
    }

    // Corners
    frame
        .buffer
        .set_fast(area.x, area.y, border_cell(set.top_left));
    frame
        .buffer
        .set_fast(right_x, area.y, border_cell(set.top_right));
    frame
        .buffer
        .set_fast(area.x, bottom_y, border_cell(set.bottom_left));
    frame
        .buffer
        .set_fast(right_x, bottom_y, border_cell(set.bottom_right));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::budget::DegradationLevel;
    use ftui_render::cell::Cell;
    use ftui_render::grapheme_pool::GraphemePool;

    fn make_snapshot(frame_id: u64, confidence: f64, in_fallback: bool) -> DriftSnapshot {
        DriftSnapshot {
            domains: vec![
                DomainSnapshot {
                    domain: DecisionDomain::DiffStrategy,
                    confidence,
                    signal: if confidence >= 0.7 {
                        TrafficLight::Green
                    } else if confidence >= 0.3 {
                        TrafficLight::Yellow
                    } else {
                        TrafficLight::Red
                    },
                    in_fallback,
                    regime_label: if in_fallback {
                        "deterministic"
                    } else {
                        "bayesian"
                    },
                },
                DomainSnapshot {
                    domain: DecisionDomain::ResizeCoalescing,
                    confidence: confidence * 0.9,
                    signal: TrafficLight::Green,
                    in_fallback: false,
                    regime_label: "bayesian",
                },
            ],
            frame_id,
        }
    }

    fn make_drift_timeline() -> DriftTimeline {
        let mut tl = DriftTimeline::new(60);
        // Normal operation (frames 0-29)
        for i in 0..30 {
            tl.push(make_snapshot(i, 0.85, false));
        }
        // Drift onset (frames 30-39)
        for i in 30..40 {
            let conf = 0.85 - (i - 30) as f64 * 0.07;
            tl.push(make_snapshot(i, conf, false));
        }
        // Fallback trigger (frames 40-49)
        for i in 40..50 {
            tl.push(make_snapshot(i, 0.15, true));
        }
        // Recovery (frames 50-59)
        for i in 50..60 {
            let conf = 0.15 + (i - 50) as f64 * 0.07;
            tl.push(make_snapshot(i, conf, false));
        }
        tl
    }

    #[test]
    fn timeline_push_and_len() {
        let mut tl = DriftTimeline::new(10);
        assert!(tl.is_empty());
        assert_eq!(tl.len(), 0);

        tl.push(make_snapshot(0, 0.8, false));
        assert_eq!(tl.len(), 1);
        assert!(!tl.is_empty());
    }

    #[test]
    fn timeline_wraps_at_capacity() {
        let mut tl = DriftTimeline::new(5);
        for i in 0..10 {
            tl.push(make_snapshot(i, 0.5, false));
        }
        assert_eq!(tl.len(), 5);

        // Latest should be frame 9
        assert_eq!(tl.latest().unwrap().frame_id, 9);
    }

    #[test]
    fn timeline_chronological_order() {
        let mut tl = DriftTimeline::new(5);
        for i in 0..8 {
            tl.push(make_snapshot(i, 0.5, false));
        }
        let ids: Vec<u64> = tl.iter_chronological().map(|s| s.frame_id).collect();
        assert_eq!(ids, vec![3, 4, 5, 6, 7]);
    }

    #[test]
    fn confidence_series_extraction() {
        let tl = make_drift_timeline();
        let series = tl.confidence_series(DecisionDomain::DiffStrategy);
        assert_eq!(series.len(), 60);
        // First value should be ~0.85 (normal)
        assert!((series[0] - 0.85).abs() < 0.01);
        // At frame 40: should be 0.15 (fallback)
        assert!((series[40] - 0.15).abs() < 0.01);
    }

    #[test]
    fn fallback_trigger_detection() {
        let tl = make_drift_timeline();
        let trigger = tl.last_fallback_trigger(DecisionDomain::DiffStrategy);
        // First fallback entry is at index 40
        assert_eq!(trigger, Some(40));
    }

    #[test]
    fn no_fallback_trigger_when_none() {
        let mut tl = DriftTimeline::new(10);
        for i in 0..10 {
            tl.push(make_snapshot(i, 0.8, false));
        }
        assert!(
            tl.last_fallback_trigger(DecisionDomain::DiffStrategy)
                .is_none()
        );
    }

    #[test]
    fn fallback_trigger_at_start_of_visible_timeline() {
        let mut tl = DriftTimeline::new(5);
        for i in 0..5 {
            tl.push(make_snapshot(i, 0.15, true));
        }

        assert_eq!(
            tl.last_fallback_trigger(DecisionDomain::DiffStrategy),
            Some(0)
        );
    }

    #[test]
    fn render_empty_timeline() {
        let tl = DriftTimeline::new(60);
        let viz = DriftVisualization::new(&tl);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        viz.render(Rect::new(0, 0, 80, 24), &mut frame);
        // Should not panic
    }

    #[test]
    fn render_populated_timeline() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 20, &mut pool);
        viz.render(Rect::new(0, 0, 80, 20), &mut frame);

        // Check title is present
        let mut found_title = false;
        for x in 0..80 {
            if let Some(cell) = frame.buffer.get(x, 1)
                && cell.content.as_char() == Some('D')
            {
                found_title = true;
                break;
            }
        }
        assert!(found_title, "should render title row");
    }

    #[test]
    fn render_no_styling_drops_border_and_label_styles() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 20, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;

        viz.render(Rect::new(0, 0, 80, 20), &mut frame);

        let border = frame.buffer.get(0, 0).unwrap();
        let border_default = Cell::from_char(border.content.as_char().unwrap());
        assert_eq!(border.fg, border_default.fg);
        assert_eq!(border.bg, border_default.bg);
        assert_eq!(border.attrs, border_default.attrs);

        let title = frame.buffer.get(1, 1).unwrap();
        let title_default = Cell::from_char('D');
        assert_eq!(title.content.as_char(), Some('D'));
        assert_eq!(title.fg, title_default.fg);
        assert_eq!(title.bg, title_default.bg);
        assert_eq!(title.attrs, title_default.attrs);

        let label = frame.buffer.get(1, 2).unwrap();
        let label_default = Cell::from_char('d');
        assert_eq!(label.content.as_char(), Some('d'));
        assert_eq!(label.fg, label_default.fg);
        assert_eq!(label.bg, label_default.bg);
        assert_eq!(label.attrs, label_default.attrs);
    }

    #[test]
    fn render_shows_fallback_indicator() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl).domains(vec![DecisionDomain::DiffStrategy]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 12, &mut pool);
        viz.render(Rect::new(0, 0, 80, 12), &mut frame);

        // Check that FALLBACK text appears (latest snapshot has in_fallback=false
        // after recovery, so we won't see FALLBACK badge on the label row)
        // but the sparkline should show the trigger marker
    }

    #[test]
    fn render_regime_banner_in_fallback() {
        // Create timeline where latest is in fallback
        let mut tl = DriftTimeline::new(10);
        for i in 0..10 {
            tl.push(make_snapshot(i, 0.15, true));
        }
        let viz = DriftVisualization::new(&tl);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 12, &mut pool);
        viz.render(Rect::new(0, 0, 80, 12), &mut frame);

        // Should contain "REGIME" text
        let mut found_regime = false;
        for y in 0..12 {
            let mut row = String::new();
            for x in 0..80 {
                if let Some(cell) = frame.buffer.get(x, y)
                    && let Some(ch) = cell.content.as_char()
                {
                    row.push(ch);
                }
            }
            if row.contains("REGIME") {
                found_regime = true;
                break;
            }
        }
        assert!(found_regime, "should show regime banner when in fallback");
    }

    #[test]
    fn render_regime_banner_normal() {
        let mut tl = DriftTimeline::new(10);
        for i in 0..10 {
            tl.push(make_snapshot(i, 0.85, false));
        }
        let viz = DriftVisualization::new(&tl);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 12, &mut pool);
        viz.render(Rect::new(0, 0, 80, 12), &mut frame);

        // Should contain "Bayesian (normal)" text
        let mut found_normal = false;
        for y in 0..12 {
            let mut row = String::new();
            for x in 0..80 {
                if let Some(cell) = frame.buffer.get(x, y)
                    && let Some(ch) = cell.content.as_char()
                {
                    row.push(ch);
                }
            }
            if row.contains("Bayesian") {
                found_normal = true;
                break;
            }
        }
        assert!(found_normal, "should show normal regime banner");
    }

    #[test]
    fn tiny_area_no_panic() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        // Should not panic with tiny area
        viz.render(Rect::new(0, 0, 5, 3), &mut frame);
    }

    #[test]
    fn min_height_calculation() {
        let mut tl = DriftTimeline::new(5);
        tl.push(make_snapshot(0, 0.8, false)); // 2 domains
        let viz = DriftVisualization::new(&tl);
        // border_top + title + 2*domain_rows + banner + border_bottom
        // = 1 + 1 + 2*2 + 1 + 1 = 8
        assert_eq!(viz.min_height(), 8);
    }

    #[test]
    fn min_height_no_banner() {
        let mut tl = DriftTimeline::new(5);
        tl.push(make_snapshot(0, 0.8, false));
        let viz = DriftVisualization::new(&tl).show_regime_banner(false);
        assert_eq!(viz.min_height(), 7);
    }

    #[test]
    fn confidence_color_zones() {
        let tl = DriftTimeline::new(1);
        let viz = DriftVisualization::new(&tl);

        let green = viz.confidence_color(0.9);
        assert_eq!(green, ZONE_GREEN);

        let red_ish = viz.confidence_color(0.1);
        // Should be near ZONE_RED
        assert!(red_ish.r() > 100);
        assert!(red_ish.g() < 100);
    }

    #[test]
    fn confidence_color_handles_degenerate_thresholds() {
        let tl = DriftTimeline::new(1);
        let viz = DriftVisualization::new(&tl)
            .fallback_threshold(0.0)
            .caution_threshold(0.0);

        assert_eq!(viz.confidence_color(0.0), ZONE_GREEN);
        let low = viz.confidence_color(-1.0);
        assert!(low.r() >= ZONE_RED.r());
    }

    #[test]
    fn builder_chain() {
        let tl = DriftTimeline::new(10);
        let viz = DriftVisualization::new(&tl)
            .border_type(BorderType::Double)
            .style(Style::new().bg(PackedRgba::rgb(10, 10, 10)))
            .show_regime_banner(false)
            .fallback_threshold(0.2)
            .caution_threshold(0.8)
            .domains(vec![DecisionDomain::DiffStrategy]);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        viz.render(Rect::new(0, 0, 80, 24), &mut frame);
    }

    #[test]
    fn is_not_essential() {
        let tl = DriftTimeline::new(1);
        let viz = DriftVisualization::new(&tl);
        assert!(!viz.is_essential());
    }

    #[test]
    fn lerp_color_endpoints() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(255, 255, 255);
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
    }

    #[test]
    fn lerp_color_clamps() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(255, 255, 255);
        assert_eq!(lerp_color(a, b, -1.0), a);
        assert_eq!(lerp_color(a, b, 2.0), b);
    }

    #[test]
    fn render_with_single_domain_filter() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl).domains(vec![DecisionDomain::DiffStrategy]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);
        viz.render(Rect::new(0, 0, 80, 10), &mut frame);

        // Should render DiffStrategy label
        let mut found_diff = false;
        for y in 0..10 {
            let mut row = String::new();
            for x in 0..80 {
                if let Some(cell) = frame.buffer.get(x, y)
                    && let Some(ch) = cell.content.as_char()
                {
                    row.push(ch);
                }
            }
            if row.contains("diff_strategy") {
                found_diff = true;
                break;
            }
        }
        assert!(found_diff, "should show DiffStrategy domain");
    }

    #[test]
    fn render_fallback_badge_on_label_row() {
        let mut tl = DriftTimeline::new(5);
        for i in 0..5 {
            tl.push(make_snapshot(i, 0.1, true));
        }
        let viz = DriftVisualization::new(&tl).domains(vec![DecisionDomain::DiffStrategy]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);
        viz.render(Rect::new(0, 0, 80, 10), &mut frame);

        let mut found_fallback = false;
        for y in 0..10 {
            let mut row = String::new();
            for x in 0..80 {
                if let Some(cell) = frame.buffer.get(x, y)
                    && let Some(ch) = cell.content.as_char()
                {
                    row.push(ch);
                }
            }
            if row.contains("FALLBACK") {
                found_fallback = true;
                break;
            }
        }
        assert!(
            found_fallback,
            "should show FALLBACK badge when in fallback"
        );
    }

    #[test]
    fn render_clears_gap_before_confidence_badge() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl).domains(vec![DecisionDomain::DiffStrategy]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);
        frame.buffer.set_fast(14, 2, Cell::from_char('X'));

        viz.render(Rect::new(0, 0, 80, 10), &mut frame);

        assert_eq!(
            frame.buffer.get(14, 2).unwrap().content.as_char(),
            Some(' ')
        );
    }

    #[test]
    fn render_skeleton_clears_previous_visualization() {
        let tl = make_drift_timeline();
        let viz = DriftVisualization::new(&tl).show_regime_banner(false);
        let area = Rect::new(0, 0, 80, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);

        viz.render(area, &mut frame);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        viz.render(area, &mut frame);

        for y in 0..area.height {
            for x in 0..area.width {
                assert_eq!(frame.buffer.get(x, y).unwrap().content.as_char(), Some(' '));
            }
        }
    }

    #[test]
    fn render_with_fewer_domains_clears_stale_rows() {
        let tl = make_drift_timeline();
        let full = DriftVisualization::new(&tl).show_regime_banner(false);
        let filtered = DriftVisualization::new(&tl)
            .domains(vec![DecisionDomain::DiffStrategy])
            .show_regime_banner(false);
        let area = Rect::new(0, 0, 80, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);

        full.render(area, &mut frame);
        filtered.render(area, &mut frame);

        for y in 4..6u16 {
            for x in 1..area.width.saturating_sub(1) {
                assert_eq!(frame.buffer.get(x, y).unwrap().content.as_char(), Some(' '));
            }
        }
    }
}
