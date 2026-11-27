#![forbid(unsafe_code)]

//! Galaxy-brain decision card widget (bd-1lg.8).
//!
//! A standalone, reusable widget that renders progressive-disclosure
//! decision transparency from the runtime's Bayesian decision engine.
//!
//! # Disclosure Levels
//!
//! - **Level 0 (Traffic Light)**: Green/yellow/red badge with action label.
//! - **Level 1 (Plain English)**: One-sentence human-readable explanation.
//! - **Level 2 (Evidence Terms)**: Bayes factors with direction indicators.
//! - **Level 3 (Full Bayesian)**: Log-posterior, CI, expected loss breakdown.
//!
//! Each level includes all information from lower levels.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::transparency::{Disclosure, disclose, DisclosureLevel};
//! use ftui_widgets::decision_card::DecisionCard;
//!
//! let disc = disclose(&decision, domain, DisclosureLevel::FullBayesian);
//! let card = DecisionCard::new(&disc);
//! card.render(area, &mut frame);
//! ```

use crate::borders::{BorderSet, BorderType};
use crate::{Widget, apply_style, clear_text_area, draw_text_span};
use ftui_a11y::node::{A11yNodeInfo, A11yRole};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::transparency::{Disclosure, DisclosureLevel, EvidenceDirection, TrafficLight};
use ftui_style::Style;

/// Traffic-light color palette.
const GREEN_FG: PackedRgba = PackedRgba::rgb(0, 200, 0);
const GREEN_BG: PackedRgba = PackedRgba::rgb(0, 60, 0);
const YELLOW_FG: PackedRgba = PackedRgba::rgb(220, 200, 0);
const YELLOW_BG: PackedRgba = PackedRgba::rgb(60, 50, 0);
const RED_FG: PackedRgba = PackedRgba::rgb(220, 50, 50);
const RED_BG: PackedRgba = PackedRgba::rgb(60, 10, 10);

const EVIDENCE_SUPPORTING_FG: PackedRgba = PackedRgba::rgb(100, 200, 100);
const EVIDENCE_OPPOSING_FG: PackedRgba = PackedRgba::rgb(200, 100, 100);
const EVIDENCE_NEUTRAL_FG: PackedRgba = PackedRgba::rgb(160, 160, 160);
const DETAIL_FG: PackedRgba = PackedRgba::rgb(140, 160, 180);
const DIM_FG: PackedRgba = PackedRgba::rgb(120, 120, 120);

/// A decision card widget showing progressive-disclosure transparency.
///
/// Renders a bordered card with traffic-light badge, explanation,
/// evidence terms, and/or full Bayesian details depending on the
/// disclosure level of the provided [`Disclosure`].
#[derive(Debug, Clone)]
pub struct DecisionCard<'a> {
    disclosure: &'a Disclosure,
    border_type: BorderType,
    style: Style,
    title_style: Style,
}

impl<'a> DecisionCard<'a> {
    /// Create a new decision card from a disclosure snapshot.
    #[must_use]
    pub fn new(disclosure: &'a Disclosure) -> Self {
        Self {
            disclosure,
            border_type: BorderType::Rounded,
            style: Style::default(),
            title_style: Style::default().bold(),
        }
    }

    /// Set the border style (Square, Rounded, Double, Heavy, Ascii).
    #[must_use]
    pub fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    /// Set the base background/foreground style for the card area.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the style for the title row.
    #[must_use]
    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = style;
        self
    }

    /// Minimum height needed to render the card at the current disclosure level.
    #[must_use]
    pub fn min_height(&self) -> u16 {
        // Border top + signal line + border bottom = 3
        let mut h: u16 = 3;
        if self.disclosure.explanation.is_some() {
            h += 1; // explanation line
        }
        if let Some(ref terms) = self.disclosure.evidence_terms
            && !terms.is_empty()
        {
            h += 1; // "Evidence:" header
            h += terms.len() as u16; // one line per term
        }
        if self.disclosure.bayesian_details.is_some() {
            h += 2; // separator + stats line
        }
        h
    }

    fn signal_style(signal: TrafficLight) -> (Style, Style) {
        let (fg, bg) = match signal {
            TrafficLight::Green => (GREEN_FG, GREEN_BG),
            TrafficLight::Yellow => (YELLOW_FG, YELLOW_BG),
            TrafficLight::Red => (RED_FG, RED_BG),
        };
        let badge_style = Style::new().fg(fg).bg(bg).bold();
        let border_style = Style::new().fg(fg);
        (badge_style, border_style)
    }

    fn render_border(&self, area: Rect, frame: &mut Frame, border_style: Style) {
        let deg = frame.buffer.degradation;
        let set = if deg.use_unicode_borders() {
            self.border_type.to_border_set()
        } else {
            BorderSet::ASCII
        };

        let border_cell = |c: char| -> Cell {
            let mut cell = Cell::from_char(c);
            apply_style(&mut cell, border_style);
            cell
        };

        // Top edge
        for x in area.x..area.right() {
            frame
                .buffer
                .set_fast(x, area.y, border_cell(set.horizontal));
        }
        // Bottom edge
        let bottom_y = area.bottom().saturating_sub(1);
        for x in area.x..area.right() {
            frame
                .buffer
                .set_fast(x, bottom_y, border_cell(set.horizontal));
        }
        // Left edge
        for y in area.y..area.bottom() {
            frame.buffer.set_fast(area.x, y, border_cell(set.vertical));
        }
        // Right edge
        let right_x = area.right().saturating_sub(1);
        for y in area.y..area.bottom() {
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

    fn render_signal_row(&self, x: u16, y: u16, max_x: u16, frame: &mut Frame) {
        let deg = frame.buffer.degradation;
        let (badge_style, _) = Self::signal_style(self.disclosure.signal);
        let badge_style = if deg.apply_styling() {
            badge_style
        } else {
            Style::default()
        };
        let title_style = if deg.apply_styling() {
            self.title_style
        } else {
            Style::default()
        };
        let label = self.disclosure.signal.label();

        // Render badge: " OK " / " WARN " / " ALERT "
        let badge_text = format!(" {label} ");
        let mut cx = draw_text_span(frame, x, y, &badge_text, badge_style, max_x);

        // Action label after badge
        if cx < max_x {
            cx = draw_text_span(frame, cx, y, " ", Style::default(), max_x);
        }
        cx = draw_text_span(
            frame,
            cx,
            y,
            &self.disclosure.action_label,
            title_style,
            max_x,
        );
        let _ = cx;
    }

    fn render_explanation(&self, x: u16, y: u16, max_x: u16, frame: &mut Frame) {
        if let Some(ref explanation) = self.disclosure.explanation {
            let style = if frame.buffer.degradation.apply_styling() {
                Style::new().fg(DIM_FG)
            } else {
                Style::default()
            };
            draw_text_span(frame, x, y, explanation, style, max_x);
        }
    }

    fn render_evidence(&self, x: u16, mut y: u16, max_x: u16, frame: &mut Frame) -> u16 {
        let terms = match self.disclosure.evidence_terms {
            Some(ref t) if !t.is_empty() => t,
            _ => return y,
        };

        let apply_styling = frame.buffer.degradation.apply_styling();
        let header_style = if apply_styling {
            Style::new().fg(DETAIL_FG).bold()
        } else {
            Style::default()
        };
        draw_text_span(frame, x, y, "Evidence:", header_style, max_x);
        y += 1;

        for term in terms {
            let (dir_char, dir_style) = match term.direction {
                EvidenceDirection::Supporting => ('+', Style::new().fg(EVIDENCE_SUPPORTING_FG)),
                EvidenceDirection::Opposing => ('-', Style::new().fg(EVIDENCE_OPPOSING_FG)),
                EvidenceDirection::Neutral => ('~', Style::new().fg(EVIDENCE_NEUTRAL_FG)),
            };
            let dir_style = if apply_styling {
                dir_style
            } else {
                Style::default()
            };
            let line = format!("  {dir_char} {}: BF={:.2}", term.label, term.bayes_factor);
            draw_text_span(frame, x, y, &line, dir_style, max_x);
            y += 1;
        }
        y
    }

    fn render_bayesian(&self, x: u16, y: u16, max_x: u16, frame: &mut Frame) {
        let details = match self.disclosure.bayesian_details {
            Some(ref d) => d,
            None => return,
        };

        let deg = frame.buffer.degradation;
        let style = if deg.apply_styling() {
            Style::new().fg(DETAIL_FG)
        } else {
            Style::default()
        };

        // Horizontal rule
        let rule_style = if deg.apply_styling() {
            Style::new().fg(DIM_FG)
        } else {
            Style::default()
        };
        let rule_len = (max_x.saturating_sub(x)) as usize;
        let rule_ch = if deg.use_unicode_borders() {
            '─'
        } else {
            '-'
        };
        let rule: String = std::iter::repeat_n(rule_ch, rule_len).collect();
        draw_text_span(frame, x, y, &rule, rule_style, max_x);

        // Stats line
        let stats = format!(
            "log_post={:.3} CI=[{:.3},{:.3}] loss={:.4} avoided={:.4}",
            details.log_posterior,
            details.confidence_interval.0,
            details.confidence_interval.1,
            details.expected_loss,
            details.loss_avoided,
        );
        draw_text_span(frame, x, y + 1, &stats, style, max_x);
    }
}

impl Widget for DecisionCard<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        if area.width < 4 || area.height < 3 {
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

        // Determine border color from signal
        let (_, border_style) = Self::signal_style(self.disclosure.signal);
        let border_style = if deg.apply_styling() {
            border_style
        } else {
            Style::default()
        };

        // Draw borders
        if deg.render_decorative() {
            self.render_border(area, frame, border_style);
        }

        // Inner area (1-cell border on each side)
        let inner_x = area.x.saturating_add(1);
        let inner_max_x = area.right().saturating_sub(1);
        let mut y = area.y.saturating_add(1);
        let max_y = area.bottom().saturating_sub(1);

        if y >= max_y || inner_x >= inner_max_x {
            return;
        }

        // Row 1: Traffic light badge + action label
        self.render_signal_row(inner_x, y, inner_max_x, frame);
        y += 1;

        // Row 2: Plain English explanation (level >= 1)
        if y < max_y && self.disclosure.level >= DisclosureLevel::PlainEnglish {
            self.render_explanation(inner_x, y, inner_max_x, frame);
            if self.disclosure.explanation.is_some() {
                y += 1;
            }
        }

        // Rows 3+: Evidence terms (level >= 2)
        if y < max_y && self.disclosure.level >= DisclosureLevel::EvidenceTerms {
            y = self.render_evidence(inner_x, y, inner_max_x, frame);
        }

        // Final rows: Full Bayesian details (level >= 3)
        if y + 1 < max_y && self.disclosure.level >= DisclosureLevel::FullBayesian {
            self.render_bayesian(inner_x, y, inner_max_x, frame);
        }

        if frame.a11y_enabled() {
            let bounds = frame.buffer.current_scissor().intersection(&area);
            if !bounds.is_empty() {
                let mut node = A11yNodeInfo::new(
                    A11yNodeInfo::stable_id_for(A11yRole::Group, area),
                    A11yRole::Group,
                    bounds,
                )
                .with_name(format!(
                    "{}: {}",
                    self.disclosure.signal.label(),
                    self.disclosure.action_label
                ));
                if let Some(ref explanation) = self.disclosure.explanation {
                    let trimmed = explanation.trim();
                    if !trimmed.is_empty() {
                        let mut chars = trimmed.chars();
                        let truncated: String = chars.by_ref().take(200).collect();
                        let desc = if chars.next().is_some() {
                            format!("{truncated}...")
                        } else {
                            truncated
                        };
                        node = node.with_description(desc);
                    }
                }
                frame.push_a11y(node);
            }
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::budget::DegradationLevel;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_runtime::transparency::{BayesianDetails, DisclosureEvidence, EvidenceDirection};
    use ftui_runtime::unified_evidence::DecisionDomain;

    fn make_disclosure(level: DisclosureLevel) -> Disclosure {
        let explanation = if level >= DisclosureLevel::PlainEnglish {
            Some("Diff strategy: chose 'full_redraw' with high confidence.".to_string())
        } else {
            None
        };

        let evidence_terms = if level >= DisclosureLevel::EvidenceTerms {
            Some(vec![
                DisclosureEvidence {
                    label: "change_rate",
                    bayes_factor: 3.5,
                    direction: EvidenceDirection::Supporting,
                },
                DisclosureEvidence {
                    label: "frame_cost",
                    bayes_factor: 0.8,
                    direction: EvidenceDirection::Opposing,
                },
                DisclosureEvidence {
                    label: "stability",
                    bayes_factor: 1.0,
                    direction: EvidenceDirection::Neutral,
                },
            ])
        } else {
            None
        };

        let bayesian_details = if level >= DisclosureLevel::FullBayesian {
            Some(BayesianDetails {
                log_posterior: 2.0,
                confidence_interval: (0.7, 0.95),
                expected_loss: 0.1,
                next_best_loss: 0.5,
                loss_avoided: 0.4,
            })
        } else {
            None
        };

        Disclosure {
            domain: DecisionDomain::DiffStrategy,
            level,
            signal: TrafficLight::Green,
            action_label: "full_redraw".to_string(),
            explanation,
            evidence_terms,
            bayesian_details,
        }
    }

    fn extract_row(frame: &Frame, y: u16, width: u16) -> String {
        let mut row = String::new();
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y) {
                if let Some(ch) = cell.content.as_char() {
                    row.push(ch);
                } else {
                    row.push(' ');
                }
            }
        }
        row
    }

    #[test]
    fn level_0_renders_badge_and_action() {
        let disc = make_disclosure(DisclosureLevel::TrafficLight);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 40, 5), &mut frame);
        let row1 = extract_row(&frame, 1, 40);
        assert!(
            row1.contains("OK"),
            "should contain traffic light label: {row1}"
        );
        assert!(
            row1.contains("full_redraw"),
            "should contain action: {row1}"
        );
    }

    #[test]
    fn level_1_includes_explanation() {
        let disc = make_disclosure(DisclosureLevel::PlainEnglish);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 6, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 60, 6), &mut frame);
        let row2 = extract_row(&frame, 2, 60);
        assert!(
            row2.contains("Diff strategy"),
            "should contain explanation: {row2}"
        );
    }

    #[test]
    fn level_2_includes_evidence() {
        let disc = make_disclosure(DisclosureLevel::EvidenceTerms);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 60, 10), &mut frame);

        let mut found_evidence = false;
        let mut found_change_rate = false;
        for y in 0..10 {
            let row = extract_row(&frame, y, 60);
            if row.contains("Evidence:") {
                found_evidence = true;
            }
            if row.contains("change_rate") {
                found_change_rate = true;
            }
        }
        assert!(found_evidence, "should show Evidence header");
        assert!(found_change_rate, "should show change_rate term");
    }

    #[test]
    fn level_3_includes_bayesian() {
        let disc = make_disclosure(DisclosureLevel::FullBayesian);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 12, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 60, 12), &mut frame);

        let mut found_log_post = false;
        for y in 0..12 {
            let row = extract_row(&frame, y, 60);
            if row.contains("log_post") {
                found_log_post = true;
            }
        }
        assert!(found_log_post, "should show log_post in Bayesian details");
    }

    #[test]
    fn tiny_area_no_panic() {
        let disc = make_disclosure(DisclosureLevel::FullBayesian);
        let mut pool = GraphemePool::new();
        // Should not panic with tiny areas
        let mut frame = Frame::new(3, 2, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 3, 2), &mut frame);
        let mut frame = Frame::new(1, 1, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 1, 1), &mut frame);
    }

    #[test]
    fn min_height_level_0() {
        let disc = make_disclosure(DisclosureLevel::TrafficLight);
        let card = DecisionCard::new(&disc);
        assert_eq!(card.min_height(), 3); // border + signal + border
    }

    #[test]
    fn min_height_level_3() {
        let disc = make_disclosure(DisclosureLevel::FullBayesian);
        let card = DecisionCard::new(&disc);
        // 3 base + 1 explanation + 1 evidence header + 3 terms + 2 bayesian = 10
        assert_eq!(card.min_height(), 10);
    }

    #[test]
    fn signal_colors_differ() {
        let (green_badge, green_border) = DecisionCard::signal_style(TrafficLight::Green);
        let (yellow_badge, _) = DecisionCard::signal_style(TrafficLight::Yellow);
        let (red_badge, red_border) = DecisionCard::signal_style(TrafficLight::Red);
        assert_ne!(green_badge.fg, yellow_badge.fg);
        assert_ne!(yellow_badge.fg, red_badge.fg);
        assert_ne!(green_border.fg, red_border.fg);
    }

    #[test]
    fn yellow_signal_shows_warn() {
        let mut disc = make_disclosure(DisclosureLevel::TrafficLight);
        disc.signal = TrafficLight::Yellow;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 40, 5), &mut frame);
        let row1 = extract_row(&frame, 1, 40);
        assert!(row1.contains("WARN"), "should contain WARN: {row1}");
    }

    #[test]
    fn red_signal_shows_alert() {
        let mut disc = make_disclosure(DisclosureLevel::TrafficLight);
        disc.signal = TrafficLight::Red;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        DecisionCard::new(&disc).render(Rect::new(0, 0, 40, 5), &mut frame);
        let row1 = extract_row(&frame, 1, 40);
        assert!(row1.contains("ALERT"), "should contain ALERT: {row1}");
    }

    #[test]
    fn builder_methods() {
        let disc = make_disclosure(DisclosureLevel::TrafficLight);
        let card = DecisionCard::new(&disc)
            .border_type(BorderType::Double)
            .style(Style::new().bg(PackedRgba::rgb(10, 10, 10)))
            .title_style(Style::new().bold());
        // Should compile and not panic when rendered
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        card.render(Rect::new(0, 0, 40, 5), &mut frame);
    }

    #[test]
    fn is_not_essential() {
        let disc = make_disclosure(DisclosureLevel::TrafficLight);
        let card = DecisionCard::new(&disc);
        assert!(!card.is_essential());
    }

    #[test]
    fn render_no_styling_drops_configured_and_signal_styles() {
        let disclosure = make_disclosure(DisclosureLevel::EvidenceTerms);
        let card = DecisionCard::new(&disclosure)
            .style(Style::new().bg(PackedRgba::rgb(10, 20, 30)))
            .title_style(Style::new().fg(PackedRgba::rgb(200, 0, 0)).bold());
        let area = Rect::new(0, 0, 40, card.min_height());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, area.height, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;

        card.render(area, &mut frame);

        let border = frame.buffer.get(0, 0).unwrap();
        let border_default = Cell::from_char(border.content.as_char().unwrap());
        assert_eq!(border.fg, border_default.fg);
        assert_eq!(border.bg, border_default.bg);
        assert_eq!(border.attrs, border_default.attrs);

        let badge = frame.buffer.get(1, 1).unwrap();
        let badge_default = Cell::from_char(' ');
        assert_eq!(badge.content.as_char(), Some(' '));
        assert_eq!(badge.fg, badge_default.fg);
        assert_eq!(badge.bg, badge_default.bg);
        assert_eq!(badge.attrs, badge_default.attrs);

        let action = frame.buffer.get(6, 1).unwrap();
        let action_default = Cell::from_char(action.content.as_char().unwrap());
        assert_eq!(action.fg, action_default.fg);
        assert_eq!(action.bg, action_default.bg);
        assert_eq!(action.attrs, action_default.attrs);
    }

    #[test]
    fn render_clears_gap_between_badge_and_action() {
        let disclosure = make_disclosure(DisclosureLevel::TrafficLight);
        let card = DecisionCard::new(&disclosure);
        let area = Rect::new(0, 0, 40, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        frame.buffer.set_fast(5, 1, Cell::from_char('X'));

        card.render(area, &mut frame);

        assert_eq!(frame.buffer.get(5, 1).unwrap().content.as_char(), Some(' '));
    }

    #[test]
    fn render_simple_borders_use_ascii_separator_rule() {
        let disclosure = make_disclosure(DisclosureLevel::FullBayesian);
        let card = DecisionCard::new(&disclosure);
        let area = Rect::new(0, 0, 60, card.min_height());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, area.height, &mut pool);
        frame.buffer.degradation = DegradationLevel::SimpleBorders;

        card.render(area, &mut frame);

        let rule_y = area.y + area.height - 3;
        assert_eq!(
            frame.buffer.get(1, rule_y).unwrap().content.as_char(),
            Some('-')
        );
    }

    #[test]
    fn render_skeleton_clears_previous_card() {
        let disclosure = make_disclosure(DisclosureLevel::FullBayesian);
        let card = DecisionCard::new(&disclosure);
        let area = Rect::new(0, 0, 60, card.min_height());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, area.height, &mut pool);

        card.render(area, &mut frame);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        card.render(area, &mut frame);

        for y in 0..area.height {
            for x in 0..area.width {
                assert_eq!(frame.buffer.get(x, y).unwrap().content.as_char(), Some(' '));
            }
        }
    }

    #[test]
    fn render_shorter_disclosure_clears_stale_rows() {
        let full = make_disclosure(DisclosureLevel::FullBayesian);
        let short = make_disclosure(DisclosureLevel::TrafficLight);
        let area = Rect::new(0, 0, 60, DecisionCard::new(&full).min_height());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, area.height, &mut pool);

        DecisionCard::new(&full).render(area, &mut frame);
        DecisionCard::new(&short).render(area, &mut frame);

        for y in 4..area.height.saturating_sub(1) {
            for x in 1..area.width.saturating_sub(1) {
                assert_eq!(frame.buffer.get(x, y).unwrap().content.as_char(), Some(' '));
            }
        }
    }
}
