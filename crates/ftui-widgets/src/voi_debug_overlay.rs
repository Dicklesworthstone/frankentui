#![forbid(unsafe_code)]

//! VOI debug overlay widget (Galaxy-Brain).

use crate::Widget;
use crate::block::{Alignment, Block};
use crate::borders::{BorderType, Borders};
use crate::paragraph::Paragraph;
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;

/// Summary of the VOI posterior.
#[derive(Debug, Clone)]
pub struct VoiPosteriorSummary {
    pub alpha: f64,
    pub beta: f64,
    pub mean: f64,
    pub variance: f64,
    pub expected_variance_after: f64,
    pub voi_gain: f64,
}

/// Summary of the most recent VOI decision.
#[derive(Debug, Clone)]
pub struct VoiDecisionSummary {
    pub event_idx: u64,
    pub should_sample: bool,
    pub reason: String,
    pub score: f64,
    pub cost: f64,
    pub log_bayes_factor: f64,
    pub e_value: f64,
    pub e_threshold: f64,
    pub boundary_score: f64,
}

/// Summary of the most recent VOI observation.
#[derive(Debug, Clone)]
pub struct VoiObservationSummary {
    pub sample_idx: u64,
    pub violated: bool,
    pub posterior_mean: f64,
    pub alpha: f64,
    pub beta: f64,
}

/// Ledger entries for the VOI debug overlay.
#[derive(Debug, Clone)]
pub enum VoiLedgerEntry {
    Decision {
        event_idx: u64,
        should_sample: bool,
        voi_gain: f64,
        log_bayes_factor: f64,
    },
    Observation {
        sample_idx: u64,
        violated: bool,
        posterior_mean: f64,
    },
}

/// Full overlay data payload.
#[derive(Debug, Clone)]
pub struct VoiOverlayData {
    pub title: String,
    pub tick: Option<u64>,
    pub source: Option<String>,
    pub posterior: VoiPosteriorSummary,
    pub decision: Option<VoiDecisionSummary>,
    pub observation: Option<VoiObservationSummary>,
    pub ledger: Vec<VoiLedgerEntry>,
}

/// Styling options for the VOI overlay.
#[derive(Debug, Clone)]
pub struct VoiOverlayStyle {
    pub border: Style,
    pub text: Style,
    pub background: Option<PackedRgba>,
    pub border_type: BorderType,
}

impl Default for VoiOverlayStyle {
    fn default() -> Self {
        Self {
            border: Style::new(),
            text: Style::new(),
            background: None,
            border_type: BorderType::Rounded,
        }
    }
}

/// VOI debug overlay widget.
#[derive(Debug, Clone)]
pub struct VoiDebugOverlay {
    data: VoiOverlayData,
    style: VoiOverlayStyle,
}

impl VoiDebugOverlay {
    /// Create a new VOI overlay widget.
    pub fn new(data: VoiOverlayData) -> Self {
        Self {
            data,
            style: VoiOverlayStyle::default(),
        }
    }

    /// Override styling for the overlay.
    #[must_use]
    pub fn with_style(mut self, style: VoiOverlayStyle) -> Self {
        self.style = style;
        self
    }

    fn build_lines(&self, line_width: usize) -> Vec<String> {
        let mut lines = Vec::with_capacity(20);
        let divider = "-".repeat(line_width);

        let mut header = self.data.title.clone();
        if let Some(tick) = self.data.tick {
            header.push_str(&format!(" (tick {})", tick));
        }
        if let Some(source) = &self.data.source {
            header.push_str(&format!(" [{source}]"));
        }

        lines.push(header);
        lines.push(divider.clone());

        if let Some(decision) = &self.data.decision {
            let verdict = if decision.should_sample {
                "SAMPLE"
            } else {
                "SKIP"
            };
            lines.push(format!(
                "Decision: {:<6}  reason: {}",
                verdict, decision.reason
            ));
            lines.push(format!(
                "log10 BF: {:+.3}  score/cost",
                decision.log_bayes_factor
            ));
            lines.push(format!(
                "E: {:.3} / {:.2}  boundary: {:.3}",
                decision.e_value, decision.e_threshold, decision.boundary_score
            ));
        } else {
            lines.push("Decision: —".to_string());
        }

        lines.push(String::new());
        lines.push("Posterior Core".to_string());
        lines.push(divider.clone());
        lines.push(format!(
            "p ~ Beta(a,b)  a={:.2}  b={:.2}",
            self.data.posterior.alpha, self.data.posterior.beta
        ));
        lines.push(format!(
            "mu={:.4}  Var={:.6}",
            self.data.posterior.mean, self.data.posterior.variance
        ));
        lines.push("VOI = Var[p] - E[Var|1]".to_string());
        lines.push(format!(
            "VOI = {:.6} - {:.6} = {:.6}",
            self.data.posterior.variance,
            self.data.posterior.expected_variance_after,
            self.data.posterior.voi_gain
        ));

        if let Some(decision) = &self.data.decision {
            lines.push(String::new());
            lines.push("Decision Equation".to_string());
            lines.push(divider.clone());
            lines.push(format!(
                "score={:.6}  cost={:.6}",
                decision.score, decision.cost
            ));
            lines.push(format!(
                "log10 BF = log10({:.6}/{:.6}) = {:+.3}",
                decision.score, decision.cost, decision.log_bayes_factor
            ));
        }

        if let Some(obs) = &self.data.observation {
            lines.push(String::new());
            lines.push("Last Sample".to_string());
            lines.push(divider.clone());
            lines.push(format!(
                "violated: {}  a={:.1}  b={:.1}  mu={:.3}",
                obs.violated, obs.alpha, obs.beta, obs.posterior_mean
            ));
        }

        if !self.data.ledger.is_empty() {
            lines.push(String::new());
            lines.push("Evidence Ledger (Recent)".to_string());
            lines.push(divider);
            for entry in &self.data.ledger {
                match entry {
                    VoiLedgerEntry::Decision {
                        event_idx,
                        should_sample,
                        voi_gain,
                        log_bayes_factor,
                    } => {
                        let verdict = if *should_sample { "S" } else { "-" };
                        lines.push(format!(
                            "D#{:>3} {verdict} VOI={:.5} logBF={:+.2}",
                            event_idx, voi_gain, log_bayes_factor
                        ));
                    }
                    VoiLedgerEntry::Observation {
                        sample_idx,
                        violated,
                        posterior_mean,
                    } => {
                        lines.push(format!(
                            "O#{:>3} viol={} mu={:.3}",
                            sample_idx, violated, posterior_mean
                        ));
                    }
                }
            }
        }

        lines
    }
}

impl Widget for VoiDebugOverlay {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() || area.width < 20 || area.height < 6 {
            return;
        }

        if let Some(bg) = self.style.background {
            let cell = Cell::default().with_bg(bg);
            frame.buffer.fill(area, cell);
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(self.style.border_type)
            .border_style(self.style.border)
            .title(&self.data.title)
            .title_alignment(Alignment::Center)
            .style(self.style.text);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let line_width = inner.width.saturating_sub(2) as usize;
        let lines = self.build_lines(line_width.max(1));
        let text = lines.join("\n");
        Paragraph::new(text)
            .style(self.style.text)
            .render(inner, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    fn sample_posterior() -> VoiPosteriorSummary {
        VoiPosteriorSummary {
            alpha: 3.2,
            beta: 7.4,
            mean: 0.301,
            variance: 0.0123,
            expected_variance_after: 0.0101,
            voi_gain: 0.0022,
        }
    }

    fn sample_data() -> VoiOverlayData {
        VoiOverlayData {
            title: "VOI Overlay".to_string(),
            tick: Some(42),
            source: Some("budget".to_string()),
            posterior: sample_posterior(),
            decision: Some(VoiDecisionSummary {
                event_idx: 7,
                should_sample: true,
                reason: "voi_gain > cost".to_string(),
                score: 0.123456,
                cost: 0.045,
                log_bayes_factor: 0.437,
                e_value: 1.23,
                e_threshold: 0.95,
                boundary_score: 0.77,
            }),
            observation: Some(VoiObservationSummary {
                sample_idx: 4,
                violated: false,
                posterior_mean: 0.312,
                alpha: 3.9,
                beta: 8.2,
            }),
            ledger: vec![
                VoiLedgerEntry::Decision {
                    event_idx: 5,
                    should_sample: true,
                    voi_gain: 0.0042,
                    log_bayes_factor: 0.31,
                },
                VoiLedgerEntry::Observation {
                    sample_idx: 3,
                    violated: true,
                    posterior_mean: 0.4,
                },
            ],
        }
    }

    #[test]
    fn build_lines_without_decision_or_ledger() {
        let data = VoiOverlayData {
            title: "VOI".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(24);

        assert!(lines[0].contains("VOI"), "header missing title: {lines:?}");
        assert_eq!(lines[1].len(), 24, "divider width mismatch: {lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("Decision: —")),
            "missing default decision line: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("Posterior Core")),
            "missing posterior section: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("Evidence Ledger")),
            "unexpected ledger section: {lines:?}"
        );
    }

    #[test]
    fn build_lines_with_decision_and_observation() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let lines = overlay.build_lines(30);

        assert!(
            lines.iter().any(|line| line.contains("Decision: SAMPLE")),
            "missing decision summary: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("Last Sample")),
            "missing observation summary: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("Evidence Ledger")),
            "missing ledger header: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("D#  5")),
            "missing decision ledger entry: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("O#  3")),
            "missing observation ledger entry: {lines:?}"
        );
    }

    #[test]
    fn render_applies_background_and_border() {
        let bg = PackedRgba::rgb(12, 34, 56);
        let style = VoiOverlayStyle {
            background: Some(bg),
            ..VoiOverlayStyle::default()
        };
        let overlay = VoiDebugOverlay::new(sample_data()).with_style(style);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 32, &mut pool);
        let area = Rect::new(0, 0, 80, 32);

        overlay.render(area, &mut frame);

        let top_left = frame.buffer.get(0, 0).unwrap();
        assert_eq!(
            top_left.content.as_char(),
            Some('╭'),
            "border not rendered as rounded: cell={top_left:?}"
        );

        let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let lines = overlay.build_lines(inner.width.saturating_sub(2) as usize);
        let extra_row = inner.y + (lines.len() as u16).saturating_add(1);
        let bg_cell = frame.buffer.get(inner.x + 1, extra_row).unwrap();
        assert_eq!(
            bg_cell.bg,
            bg,
            "background not applied at ({}, {}): cell={bg_cell:?}",
            inner.x + 1,
            extra_row
        );
    }

    #[test]
    fn render_small_area_noop() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 4, &mut pool);
        let before = frame.buffer.get(0, 0).copied();

        overlay.render(Rect::new(0, 0, 10, 4), &mut frame);

        let after = frame.buffer.get(0, 0).copied();
        assert_eq!(
            before, after,
            "small-area render should be no-op: before={before:?} after={after:?}"
        );
    }

    // --- Style defaults ---

    #[test]
    fn overlay_style_default() {
        let style = VoiOverlayStyle::default();
        assert!(style.background.is_none());
        assert!(matches!(style.border_type, BorderType::Rounded));
    }

    // --- Header formatting ---

    #[test]
    fn build_lines_header_with_tick_and_source() {
        let data = VoiOverlayData {
            title: "Test".to_string(),
            tick: Some(100),
            source: Some("resize".to_string()),
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(40);
        assert!(lines[0].contains("Test (tick 100) [resize]"));
    }

    #[test]
    fn build_lines_header_no_tick_no_source() {
        let data = VoiOverlayData {
            title: "Plain".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(20);
        assert_eq!(lines[0], "Plain");
    }

    // --- Decision verdict ---

    #[test]
    fn build_lines_skip_verdict() {
        let data = VoiOverlayData {
            title: "Test".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: Some(VoiDecisionSummary {
                event_idx: 1,
                should_sample: false,
                reason: "cost_too_high".to_string(),
                score: 0.01,
                cost: 0.1,
                log_bayes_factor: -1.0,
                e_value: 0.5,
                e_threshold: 0.95,
                boundary_score: 0.2,
            }),
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(40);
        assert!(
            lines.iter().any(|l| l.contains("Decision: SKIP")),
            "expected SKIP verdict: {lines:?}"
        );
    }

    // --- Observation only (no decision) ---

    #[test]
    fn build_lines_observation_only() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: Some(VoiObservationSummary {
                sample_idx: 10,
                violated: true,
                posterior_mean: 0.456,
                alpha: 5.0,
                beta: 10.0,
            }),
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(40);
        assert!(
            lines.iter().any(|l| l.contains("violated: true")),
            "missing violated observation: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("mu=0.456")),
            "missing posterior mean: {lines:?}"
        );
    }

    // --- Ledger formatting ---

    #[test]
    fn build_lines_ledger_skip_entry() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: vec![VoiLedgerEntry::Decision {
                event_idx: 99,
                should_sample: false,
                voi_gain: 0.001,
                log_bayes_factor: -0.5,
            }],
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(40);
        assert!(
            lines.iter().any(|l| l.contains("D# 99 -")),
            "expected skip marker: {lines:?}"
        );
    }

    // --- Posterior formatting ---

    #[test]
    fn build_lines_posterior_values() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: VoiPosteriorSummary {
                alpha: 1.0,
                beta: 1.0,
                mean: 0.5,
                variance: 0.0833,
                expected_variance_after: 0.0500,
                voi_gain: 0.0333,
            },
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(40);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("a=1.00") && l.contains("b=1.00")),
            "missing alpha/beta: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("mu=0.5000")),
            "missing mean: {lines:?}"
        );
    }

    // --- with_style builder ---

    #[test]
    fn with_style_replaces_style() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let custom = VoiOverlayStyle {
            background: Some(PackedRgba::rgb(255, 0, 0)),
            border_type: BorderType::Square,
            ..VoiOverlayStyle::default()
        };
        let styled = overlay.with_style(custom);
        assert_eq!(styled.style.background, Some(PackedRgba::rgb(255, 0, 0)));
    }

    #[test]
    fn render_empty_area_is_noop() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);

        // Zero-width area
        overlay.render(Rect::new(0, 0, 0, 10), &mut frame);
        // Zero-height area
        overlay.render(Rect::new(0, 0, 40, 0), &mut frame);
        // Both zero — should not panic
    }

    #[test]
    fn render_narrow_area_where_inner_is_empty() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 40, &mut pool);
        // Area has borders consuming all space (width=20 passes threshold, height=6 passes)
        // Inner is 18x4 which is non-empty, so use exactly at threshold
        overlay.render(Rect::new(0, 0, 20, 6), &mut frame);
        // Should render without panic
    }

    #[test]
    fn build_lines_ledger_observation_entry() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: vec![VoiLedgerEntry::Observation {
                sample_idx: 42,
                violated: false,
                posterior_mean: 0.789,
            }],
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(40);
        assert!(
            lines.iter().any(|l| l.contains("O# 42")),
            "missing observation ledger entry: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("viol=false")),
            "missing violated=false: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("mu=0.789")),
            "missing posterior mean in ledger: {lines:?}"
        );
    }

    #[test]
    fn build_lines_decision_equation_section() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let lines = overlay.build_lines(50);
        assert!(
            lines.iter().any(|l| l.contains("Decision Equation")),
            "missing decision equation header: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("score=") && l.contains("cost=")),
            "missing score/cost line: {lines:?}"
        );
    }

    #[test]
    fn build_lines_voi_equation_format() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: VoiPosteriorSummary {
                alpha: 2.0,
                beta: 3.0,
                mean: 0.4,
                variance: 0.04,
                expected_variance_after: 0.03,
                voi_gain: 0.01,
            },
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(50);
        // VOI = Var[p] - E[Var|1] header
        assert!(
            lines.iter().any(|l| l.contains("VOI = Var[p] - E[Var|1]")),
            "missing VOI equation label: {lines:?}"
        );
        // VOI = 0.040000 - 0.030000 = 0.010000
        assert!(
            lines.iter().any(|l| l.contains("0.040000")
                && l.contains("0.030000")
                && l.contains("0.010000")),
            "missing VOI computation line: {lines:?}"
        );
    }

    #[test]
    fn overlay_data_clone() {
        let data = sample_data();
        let cloned = data.clone();
        assert_eq!(cloned.title, data.title);
        assert_eq!(cloned.tick, data.tick);
        assert_eq!(cloned.ledger.len(), data.ledger.len());
    }

    // ─── Edge-case tests (bd-3szd1) ────────────────────────────────────

    #[test]
    fn build_lines_width_zero() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let lines = overlay.build_lines(0);
        // Should not panic; divider is empty string
        assert!(!lines.is_empty());
    }

    #[test]
    fn build_lines_width_one() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let lines = overlay.build_lines(1);
        assert_eq!(lines[1], "-", "divider should be single dash");
    }

    #[test]
    fn build_lines_empty_title() {
        let data = VoiOverlayData {
            title: String::new(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(20);
        assert_eq!(lines[0], "", "empty title should produce empty header");
    }

    #[test]
    fn build_lines_tick_only_no_source() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: Some(0),
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(30);
        assert!(lines[0].contains("(tick 0)"));
        assert!(!lines[0].contains('['));
    }

    #[test]
    fn build_lines_source_only_no_tick() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: Some("src".to_string()),
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(30);
        assert!(lines[0].contains("[src]"));
        assert!(!lines[0].contains("tick"));
    }

    #[test]
    fn render_width_below_threshold() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 40, &mut pool);
        // width=19 is below the 20 threshold
        overlay.render(Rect::new(0, 0, 19, 10), &mut frame);
        // Should be noop — verify no border rendered
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_ne!(
            cell.content.as_char(),
            Some('╭'),
            "should not render border at width=19"
        );
    }

    #[test]
    fn render_height_below_threshold() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 40, &mut pool);
        // height=5 is below the 6 threshold
        overlay.render(Rect::new(0, 0, 40, 5), &mut frame);
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_ne!(
            cell.content.as_char(),
            Some('╭'),
            "should not render border at height=5"
        );
    }

    #[test]
    fn render_exact_minimum_size() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 40, &mut pool);
        // Exactly at threshold: width=20, height=6
        overlay.render(Rect::new(0, 0, 20, 6), &mut frame);
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(
            cell.content.as_char(),
            Some('╭'),
            "should render border at exact minimum size"
        );
    }

    #[test]
    fn posterior_with_nan_values() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: VoiPosteriorSummary {
                alpha: f64::NAN,
                beta: f64::INFINITY,
                mean: f64::NEG_INFINITY,
                variance: 0.0,
                expected_variance_after: 0.0,
                voi_gain: -0.0,
            },
            decision: None,
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(50);
        // Should format without panic
        assert!(
            lines.iter().any(|l| l.contains("NaN") || l.contains("nan")),
            "NaN alpha should appear in output: {lines:?}"
        );
    }

    #[test]
    fn large_event_idx_in_ledger() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: vec![VoiLedgerEntry::Decision {
                event_idx: u64::MAX,
                should_sample: true,
                voi_gain: 0.0,
                log_bayes_factor: 0.0,
            }],
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(80);
        assert!(
            lines.iter().any(|l| l.contains(&u64::MAX.to_string())),
            "large event_idx should appear: {lines:?}"
        );
    }

    #[test]
    fn multiple_ledger_entries_same_type() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: None,
            observation: None,
            ledger: vec![
                VoiLedgerEntry::Decision {
                    event_idx: 1,
                    should_sample: true,
                    voi_gain: 0.01,
                    log_bayes_factor: 0.5,
                },
                VoiLedgerEntry::Decision {
                    event_idx: 2,
                    should_sample: false,
                    voi_gain: 0.001,
                    log_bayes_factor: -0.3,
                },
                VoiLedgerEntry::Decision {
                    event_idx: 3,
                    should_sample: true,
                    voi_gain: 0.02,
                    log_bayes_factor: 1.0,
                },
            ],
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(50);
        let decision_lines: Vec<_> = lines.iter().filter(|l| l.starts_with("D#")).collect();
        assert_eq!(decision_lines.len(), 3, "expected 3 decision entries");
    }

    #[test]
    fn negative_log_bayes_factor_format() {
        let data = VoiOverlayData {
            title: "T".to_string(),
            tick: None,
            source: None,
            posterior: sample_posterior(),
            decision: Some(VoiDecisionSummary {
                event_idx: 1,
                should_sample: false,
                reason: "negative".to_string(),
                score: 0.001,
                cost: 0.1,
                log_bayes_factor: -2.345,
                e_value: 0.1,
                e_threshold: 0.95,
                boundary_score: 0.05,
            }),
            observation: None,
            ledger: Vec::new(),
        };
        let overlay = VoiDebugOverlay::new(data);
        let lines = overlay.build_lines(50);
        assert!(
            lines.iter().any(|l| l.contains("-2.345")),
            "negative log BF should appear: {lines:?}"
        );
    }

    #[test]
    fn voi_ledger_entry_clone() {
        let entry = VoiLedgerEntry::Decision {
            event_idx: 5,
            should_sample: true,
            voi_gain: 0.01,
            log_bayes_factor: 0.5,
        };
        let cloned = entry.clone();
        assert!(format!("{cloned:?}").contains("Decision"));
    }

    #[test]
    fn voi_decision_summary_clone() {
        let d = VoiDecisionSummary {
            event_idx: 1,
            should_sample: true,
            reason: "test".to_string(),
            score: 1.0,
            cost: 0.5,
            log_bayes_factor: 0.3,
            e_value: 1.0,
            e_threshold: 0.95,
            boundary_score: 0.5,
        };
        let cloned = d.clone();
        assert_eq!(cloned.reason, "test");
        assert_eq!(cloned.event_idx, 1);
    }

    #[test]
    fn voi_observation_summary_clone() {
        let o = VoiObservationSummary {
            sample_idx: 42,
            violated: true,
            posterior_mean: 0.5,
            alpha: 3.0,
            beta: 7.0,
        };
        let cloned = o.clone();
        assert!(cloned.violated);
        assert_eq!(cloned.sample_idx, 42);
    }

    #[test]
    fn with_style_custom_border_type() {
        let overlay = VoiDebugOverlay::new(sample_data()).with_style(VoiOverlayStyle {
            border_type: BorderType::Double,
            ..VoiOverlayStyle::default()
        });
        assert!(matches!(overlay.style.border_type, BorderType::Double));
    }

    #[test]
    fn render_no_background() {
        let data = sample_data();
        let overlay = VoiDebugOverlay::new(data);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 32, &mut pool);
        // Default style has no background
        overlay.render(Rect::new(0, 0, 80, 32), &mut frame);
        // Should render border without panic
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('╭'));
    }

    #[test]
    fn build_lines_divider_matches_width() {
        let overlay = VoiDebugOverlay::new(sample_data());
        let width = 37;
        let lines = overlay.build_lines(width);
        // line[1] is the first divider
        assert_eq!(
            lines[1].len(),
            width,
            "divider should match requested width"
        );
    }

    // ─── End edge-case tests (bd-3szd1) ──────────────────────────────

    // --- Struct Debug impls ---

    #[test]
    fn structs_implement_debug() {
        let posterior = sample_posterior();
        let _ = format!("{posterior:?}");

        let data = sample_data();
        let _ = format!("{data:?}");

        let overlay = VoiDebugOverlay::new(data);
        let _ = format!("{overlay:?}");

        let style = VoiOverlayStyle::default();
        let _ = format!("{style:?}");
    }
}
