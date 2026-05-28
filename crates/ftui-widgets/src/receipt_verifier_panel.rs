#![forbid(unsafe_code)]

//! Receipt verifier panel widget (bd-cixqu.1.8 / Track A.7.1).
//!
//! Renders the operator-facing verifier verdict for a signed decision receipt,
//! mirroring `runbooks/scripts/verify_receipt.sh` inside the TUI. The panel
//! takes a parsed [`ReceiptVerdict`] (build it from the verifier's JSON
//! output, or programmatically) and displays:
//!
//! - the overall verdict (receipt id + PASS/FAIL + failure_class on failure);
//! - the three verification layers (signature / transparency / attestation)
//!   each as PASS/FAIL with its `error_code`;
//! - optional posterior path the receipt binds (`show_posterior_path`);
//! - optional transparency-log inclusion + signature evidence chain
//!   (`show_evidence_chain`);
//! - warnings; and a triage block keyed off the `failure_class`.
//!
//! The panel intentionally keeps its data model self-contained (no engine
//! dependency) so callers can construct it from any source — the verifier's
//! `UnifiedReceiptVerificationVerdict` JSON, fixtures, mocks for previews,
//! etc. The `verify_receipt.sh` verdict shape maps field-for-field onto the
//! types defined here.

use crate::borders::{BorderSet, BorderType};
use crate::{Widget, apply_style, clear_text_area, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;

// ---------------------------------------------------------------------------
// Colour palette (mirrors decision_card)
// ---------------------------------------------------------------------------

const PASS_FG: PackedRgba = PackedRgba::rgb(0, 200, 0);
const PASS_BG: PackedRgba = PackedRgba::rgb(0, 60, 0);
const FAIL_FG: PackedRgba = PackedRgba::rgb(220, 50, 50);
const FAIL_BG: PackedRgba = PackedRgba::rgb(60, 10, 10);
const WARN_FG: PackedRgba = PackedRgba::rgb(220, 200, 0);
const HEADER_FG: PackedRgba = PackedRgba::rgb(140, 160, 180);
const DIM_FG: PackedRgba = PackedRgba::rgb(120, 120, 120);
const DETAIL_FG: PackedRgba = PackedRgba::rgb(160, 180, 200);

// ---------------------------------------------------------------------------
// Data model — mirrors the verifier verdict JSON shape
// ---------------------------------------------------------------------------

/// Outcome of a single layer-internal check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail,
    Warn,
}

impl CheckOutcome {
    /// Parse an `outcome` string as emitted by the verifier (`pass`/`fail`/`warn`).
    /// Unknown values are treated as `Warn` so they remain visible.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "pass" | "PASS" => Self::Pass,
            "fail" | "FAIL" => Self::Fail,
            _ => Self::Warn,
        }
    }

    /// Short label suitable for rendering (`PASS` / `FAIL` / `WARN`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Warn => "WARN",
        }
    }
}

/// One layer-internal check (signature/transparency/attestation sub-check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckEntry {
    pub check: String,
    pub outcome: CheckOutcome,
    pub error_code: Option<String>,
    pub detail: String,
}

/// Verdict for one of the three verification layers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayerVerdict {
    pub passed: bool,
    pub error_code: Option<String>,
    pub checks: Vec<CheckEntry>,
}

/// Failure classification when `passed == false`.
///
/// Matches the verifier's `failure_class` field — `None` means the receipt
/// verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Signature,
    Transparency,
    Attestation,
    StaleData,
}

impl FailureClass {
    /// Parse the JSON failure_class string. Returns `None` for null/missing.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Signature" => Some(Self::Signature),
            "Transparency" => Some(Self::Transparency),
            "Attestation" => Some(Self::Attestation),
            "StaleData" => Some(Self::StaleData),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Signature => "Signature",
            Self::Transparency => "Transparency",
            Self::Attestation => "Attestation",
            Self::StaleData => "StaleData",
        }
    }
}

/// Optional posterior snapshot the receipt binds (from the input JSON).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosteriorSnapshot {
    pub point_estimate: Option<f64>,
    pub ci_lower: Option<f64>,
    pub ci_upper: Option<f64>,
}

/// A parsed verifier verdict ready for the panel to render.
///
/// Maps onto the `UnifiedReceiptVerificationVerdict` shape that
/// `frankenctl verify receipt --output` emits.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptVerdict {
    pub receipt_id: String,
    pub trace_id: String,
    pub decision_id: String,
    pub policy_id: String,
    pub verification_timestamp_ns: u64,
    pub passed: bool,
    pub failure_class: Option<FailureClass>,
    pub exit_code: i32,
    pub signature: LayerVerdict,
    pub transparency: LayerVerdict,
    pub attestation: LayerVerdict,
    pub warnings: Vec<String>,
    /// Optional — populated from the verifier input JSON when available.
    pub posterior_snapshot: Option<PosteriorSnapshot>,
}

impl ReceiptVerdict {
    /// Build a minimal verdict skeleton (mostly for tests / previews).
    #[must_use]
    pub fn skeleton(receipt_id: impl Into<String>, passed: bool) -> Self {
        Self {
            receipt_id: receipt_id.into(),
            trace_id: String::new(),
            decision_id: String::new(),
            policy_id: String::new(),
            verification_timestamp_ns: 0,
            passed,
            failure_class: None,
            exit_code: if passed { 0 } else { 2 },
            signature: LayerVerdict::default(),
            transparency: LayerVerdict::default(),
            attestation: LayerVerdict::default(),
            warnings: Vec::new(),
            posterior_snapshot: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Panel widget
// ---------------------------------------------------------------------------

/// Receipt verifier panel — renders a [`ReceiptVerdict`] inside a bordered
/// frame.
///
/// # Usage
///
/// ```ignore
/// use ftui_widgets::receipt_verifier_panel::{ReceiptVerifierPanel, ReceiptVerdict};
///
/// let verdict = ReceiptVerdict::skeleton("rcpt-001", true);
/// let panel = ReceiptVerifierPanel::new(&verdict)
///     .show_posterior_path(true)
///     .show_evidence_chain(true);
/// panel.render(area, &mut frame);
/// ```
#[derive(Debug, Clone)]
pub struct ReceiptVerifierPanel<'a> {
    verdict: &'a ReceiptVerdict,
    border_type: BorderType,
    style: Style,
    title_style: Style,
    show_posterior_path: bool,
    show_evidence_chain: bool,
    show_triage: bool,
}

impl<'a> ReceiptVerifierPanel<'a> {
    /// Create a new panel for the given verdict.
    ///
    /// Defaults: rounded border, posterior path and evidence chain hidden,
    /// triage block shown on failure.
    #[must_use]
    pub fn new(verdict: &'a ReceiptVerdict) -> Self {
        Self {
            verdict,
            border_type: BorderType::Rounded,
            style: Style::default(),
            title_style: Style::default().bold(),
            show_posterior_path: false,
            show_evidence_chain: false,
            show_triage: true,
        }
    }

    #[must_use]
    pub fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = style;
        self
    }

    /// Show the decision posterior snapshot the receipt binds, when available
    /// in [`ReceiptVerdict::posterior_snapshot`].
    #[must_use]
    pub fn show_posterior_path(mut self, show: bool) -> Self {
        self.show_posterior_path = show;
        self
    }

    /// Show the transparency-log + signature evidence-chain checks.
    #[must_use]
    pub fn show_evidence_chain(mut self, show: bool) -> Self {
        self.show_evidence_chain = show;
        self
    }

    /// Show the failure-class triage block on failure. On by default.
    #[must_use]
    pub fn show_triage(mut self, show: bool) -> Self {
        self.show_triage = show;
        self
    }

    /// Minimum height needed to render the panel given the current options.
    #[must_use]
    pub fn min_height(&self) -> u16 {
        // Border top + verdict row + provenance row + 3 layer rows + border bottom = 7.
        let mut h: u16 = 7;
        if self.show_posterior_path
            && let Some(snapshot) = self.verdict.posterior_snapshot.as_ref()
        {
            // header + up to 3 numeric rows
            h += 1 + posterior_visible_rows(snapshot);
        }
        if self.show_evidence_chain {
            // header + per-layer line + each check line for signature+transparency
            h += 1 + 2; // headers for transparency + signature blocks
            h += self.verdict.transparency.checks.len() as u16;
            h += self.verdict.signature.checks.len() as u16;
        }
        if !self.verdict.warnings.is_empty() {
            h += 1 + self.verdict.warnings.len() as u16; // header + per-warning
        }
        if self.show_triage
            && let Some(fc) = self.verdict.failure_class
        {
            h += triage_height(fc);
        }
        h
    }

    fn border_style_for_status(passed: bool) -> Style {
        if passed {
            Style::new().fg(PASS_FG)
        } else {
            Style::new().fg(FAIL_FG)
        }
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

        for x in area.x..area.right() {
            frame
                .buffer
                .set_fast(x, area.y, border_cell(set.horizontal));
        }
        let bottom_y = area.bottom().saturating_sub(1);
        for x in area.x..area.right() {
            frame
                .buffer
                .set_fast(x, bottom_y, border_cell(set.horizontal));
        }
        for y in area.y..area.bottom() {
            frame.buffer.set_fast(area.x, y, border_cell(set.vertical));
        }
        let right_x = area.right().saturating_sub(1);
        for y in area.y..area.bottom() {
            frame.buffer.set_fast(right_x, y, border_cell(set.vertical));
        }
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

    fn verdict_badge_style(passed: bool, apply_styling: bool) -> Style {
        if !apply_styling {
            return Style::default();
        }
        if passed {
            Style::new().fg(PASS_FG).bg(PASS_BG).bold()
        } else {
            Style::new().fg(FAIL_FG).bg(FAIL_BG).bold()
        }
    }

    fn render_verdict_row(&self, x: u16, y: u16, max_x: u16, frame: &mut Frame) {
        let apply_styling = frame.buffer.degradation.apply_styling();
        let badge_text = if self.verdict.passed {
            " VERIFIED "
        } else {
            " FAILED "
        };
        let badge_style = Self::verdict_badge_style(self.verdict.passed, apply_styling);
        let title_style = if apply_styling {
            self.title_style
        } else {
            Style::default()
        };

        let mut cx = draw_text_span(frame, x, y, badge_text, badge_style, max_x);
        if cx < max_x {
            cx = draw_text_span(frame, cx, y, " ", Style::default(), max_x);
        }
        let id_text = format!("receipt {}", self.verdict.receipt_id);
        cx = draw_text_span(frame, cx, y, &id_text, title_style, max_x);

        if !self.verdict.passed
            && let Some(fc) = self.verdict.failure_class
        {
            let detail_style = if apply_styling {
                Style::new().fg(FAIL_FG)
            } else {
                Style::default()
            };
            let trail = format!(
                "   failure_class={}  exit_code={}",
                fc.label(),
                self.verdict.exit_code
            );
            let _ = draw_text_span(frame, cx, y, &trail, detail_style, max_x);
        }
    }

    fn render_provenance_row(&self, x: u16, y: u16, max_x: u16, frame: &mut Frame) {
        let style = if frame.buffer.degradation.apply_styling() {
            Style::new().fg(DETAIL_FG)
        } else {
            Style::default()
        };
        let line = format!(
            "trace={}  decision={}  policy={}",
            display_or_dash(&self.verdict.trace_id),
            display_or_dash(&self.verdict.decision_id),
            display_or_dash(&self.verdict.policy_id),
        );
        draw_text_span(frame, x, y, &line, style, max_x);
    }

    fn render_layer_row(
        x: u16,
        y: u16,
        max_x: u16,
        frame: &mut Frame,
        name: &str,
        layer: &LayerVerdict,
    ) {
        let apply_styling = frame.buffer.degradation.apply_styling();
        let label = format!("  {name:<13} ");
        let label_style = if apply_styling {
            Style::new().fg(HEADER_FG)
        } else {
            Style::default()
        };
        let mut cx = draw_text_span(frame, x, y, &label, label_style, max_x);

        let outcome_label = if layer.passed { "PASS" } else { "FAIL" };
        let outcome_style = if !apply_styling {
            Style::default()
        } else if layer.passed {
            Style::new().fg(PASS_FG).bold()
        } else {
            Style::new().fg(FAIL_FG).bold()
        };
        cx = draw_text_span(frame, cx, y, outcome_label, outcome_style, max_x);

        let trail_style = if apply_styling {
            Style::new().fg(DIM_FG)
        } else {
            Style::default()
        };
        let trail = format!(
            "   error_code={}",
            layer.error_code.as_deref().unwrap_or("-")
        );
        let _ = draw_text_span(frame, cx, y, &trail, trail_style, max_x);
    }

    fn render_posterior_path(&self, x: u16, mut y: u16, max_x: u16, frame: &mut Frame) -> u16 {
        let Some(snapshot) = self.verdict.posterior_snapshot.as_ref() else {
            return y;
        };
        let apply_styling = frame.buffer.degradation.apply_styling();
        let header_style = if apply_styling {
            Style::new().fg(HEADER_FG).bold()
        } else {
            Style::default()
        };
        let detail_style = if apply_styling {
            Style::new().fg(DETAIL_FG)
        } else {
            Style::default()
        };

        draw_text_span(frame, x, y, "posterior path:", header_style, max_x);
        y += 1;
        if let Some(p) = snapshot.point_estimate {
            draw_text_span(
                frame,
                x,
                y,
                &format!("  point_estimate = {p}"),
                detail_style,
                max_x,
            );
            y += 1;
        }
        if let Some(lo) = snapshot.ci_lower {
            draw_text_span(
                frame,
                x,
                y,
                &format!("  confidence_interval_95_lower = {lo}"),
                detail_style,
                max_x,
            );
            y += 1;
        }
        if let Some(hi) = snapshot.ci_upper {
            draw_text_span(
                frame,
                x,
                y,
                &format!("  confidence_interval_95_upper = {hi}"),
                detail_style,
                max_x,
            );
            y += 1;
        }
        y
    }

    fn render_evidence_chain(&self, x: u16, mut y: u16, max_x: u16, frame: &mut Frame) -> u16 {
        let apply_styling = frame.buffer.degradation.apply_styling();
        let header_style = if apply_styling {
            Style::new().fg(HEADER_FG).bold()
        } else {
            Style::default()
        };

        draw_text_span(frame, x, y, "evidence chain:", header_style, max_x);
        y += 1;
        y = render_layer_checks(
            x,
            y,
            max_x,
            frame,
            "transparency",
            &self.verdict.transparency,
        );
        y = render_layer_checks(x, y, max_x, frame, "signature", &self.verdict.signature);
        y
    }

    fn render_warnings(&self, x: u16, mut y: u16, max_x: u16, frame: &mut Frame) -> u16 {
        if self.verdict.warnings.is_empty() {
            return y;
        }
        let apply_styling = frame.buffer.degradation.apply_styling();
        let header_style = if apply_styling {
            Style::new().fg(WARN_FG).bold()
        } else {
            Style::default()
        };
        let line_style = if apply_styling {
            Style::new().fg(WARN_FG)
        } else {
            Style::default()
        };
        draw_text_span(frame, x, y, "warnings:", header_style, max_x);
        y += 1;
        for w in &self.verdict.warnings {
            draw_text_span(frame, x, y, &format!("  ! {w}"), line_style, max_x);
            y += 1;
        }
        y
    }

    fn render_triage(&self, x: u16, mut y: u16, max_x: u16, frame: &mut Frame) -> u16 {
        if !self.show_triage {
            return y;
        }
        let Some(fc) = self.verdict.failure_class else {
            return y;
        };
        let apply_styling = frame.buffer.degradation.apply_styling();
        let header_style = if apply_styling {
            Style::new().fg(FAIL_FG).bold()
        } else {
            Style::default()
        };
        let body_style = if apply_styling {
            Style::new().fg(DETAIL_FG)
        } else {
            Style::default()
        };

        draw_text_span(
            frame,
            x,
            y,
            &format!("incident triage (failure_class={}):", fc.label()),
            header_style,
            max_x,
        );
        y += 1;
        for line in triage_lines(fc) {
            draw_text_span(frame, x, y, line, body_style, max_x);
            y += 1;
        }
        y
    }
}

impl Widget for ReceiptVerifierPanel<'_> {
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

        let border_style = Self::border_style_for_status(self.verdict.passed);
        let border_style = if deg.apply_styling() {
            border_style
        } else {
            Style::default()
        };
        if deg.render_decorative() {
            self.render_border(area, frame, border_style);
        }

        let inner_x = area.x.saturating_add(1);
        let inner_max_x = area.right().saturating_sub(1);
        let mut y = area.y.saturating_add(1);
        let max_y = area.bottom().saturating_sub(1);

        if y >= max_y || inner_x >= inner_max_x {
            return;
        }

        // 1. Verdict row.
        self.render_verdict_row(inner_x, y, inner_max_x, frame);
        y += 1;
        if y >= max_y {
            return;
        }

        // 2. Provenance row.
        self.render_provenance_row(inner_x, y, inner_max_x, frame);
        y += 1;
        if y >= max_y {
            return;
        }

        // 3. Three-layer status block.
        for (name, layer) in [
            ("signature", &self.verdict.signature),
            ("transparency", &self.verdict.transparency),
            ("attestation", &self.verdict.attestation),
        ] {
            if y >= max_y {
                return;
            }
            Self::render_layer_row(inner_x, y, inner_max_x, frame, name, layer);
            y += 1;
        }

        // 4. Optional posterior path.
        if self.show_posterior_path && self.verdict.posterior_snapshot.is_some() && y < max_y {
            y = self.render_posterior_path(inner_x, y, inner_max_x, frame);
        }

        // 5. Optional evidence chain.
        if self.show_evidence_chain && y < max_y {
            y = self.render_evidence_chain(inner_x, y, inner_max_x, frame);
        }

        // 6. Warnings.
        if y < max_y {
            y = self.render_warnings(inner_x, y, inner_max_x, frame);
        }

        // 7. Triage.
        if y < max_y {
            let _ = self.render_triage(inner_x, y, inner_max_x, frame);
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn display_or_dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

fn render_layer_checks(
    x: u16,
    mut y: u16,
    max_x: u16,
    frame: &mut Frame,
    name: &str,
    layer: &LayerVerdict,
) -> u16 {
    let apply_styling = frame.buffer.degradation.apply_styling();
    let header_style = if apply_styling {
        Style::new().fg(HEADER_FG)
    } else {
        Style::default()
    };
    let verdict = if layer.passed { "PASS" } else { "FAIL" };
    draw_text_span(
        frame,
        x,
        y,
        &format!("  {name} layer ({verdict}):"),
        header_style,
        max_x,
    );
    y += 1;
    for check in &layer.checks {
        let outcome_style = if !apply_styling {
            Style::default()
        } else {
            match check.outcome {
                CheckOutcome::Pass => Style::new().fg(PASS_FG),
                CheckOutcome::Fail => Style::new().fg(FAIL_FG),
                CheckOutcome::Warn => Style::new().fg(WARN_FG),
            }
        };
        let line = format!(
            "    [{}] {} — {}",
            check.outcome.label(),
            check.check,
            check.detail
        );
        draw_text_span(frame, x, y, &line, outcome_style, max_x);
        y += 1;
    }
    y
}

fn posterior_visible_rows(snapshot: &PosteriorSnapshot) -> u16 {
    let mut n: u16 = 0;
    if snapshot.point_estimate.is_some() {
        n += 1;
    }
    if snapshot.ci_lower.is_some() {
        n += 1;
    }
    if snapshot.ci_upper.is_some() {
        n += 1;
    }
    n
}

/// Operator-facing triage lines per failure class. Mirrors the prose emitted
/// by `verify_receipt.sh print_triage`.
fn triage_lines(fc: FailureClass) -> &'static [&'static str] {
    match fc {
        FailureClass::Signature => &[
            "  The receipt's threshold signature did not validate.",
            "  -> Confirm the verifier has the correct signer verification keys.",
            "  -> A genuine mismatch means the receipt was not produced by the",
            "     attested signing quorum: treat the decision as UNTRUSTED.",
        ],
        FailureClass::Transparency => &[
            "  The transparency-log inclusion and/or consistency proof failed.",
            "  -> --show-evidence-chain shows which MMR proof check failed.",
            "  -> Inclusion fail: the receipt is not in the published log",
            "     (possible equivocation/omission). Consistency fail: the log was",
            "     forked or rewritten between checkpoints. Escalate to log-operator.",
        ],
        FailureClass::Attestation => &[
            "  The TEE attestation quote could not be validated.",
            "  -> Runtime has degraded to SAFE-MODE: it continues only under the",
            "     restricted capability posture (no attested-only operations),",
            "     because it can no longer prove it runs in a trusted enclave.",
            "  -> Check tee_attestation_policy freshness/measurement; a stale or",
            "     mismatched measurement is the usual cause. Re-attest before",
            "     promoting any decision that requires a trusted enclave.",
        ],
        FailureClass::StaleData => &[
            "  Verifier input is stale (timestamp/epoch outside accepted window).",
            "  -> Re-export a fresh verifier_input.json for this receipt and re-run.",
        ],
    }
}

fn triage_height(fc: FailureClass) -> u16 {
    1 + triage_lines(fc).len() as u16
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    fn make_pass() -> ReceiptVerdict {
        let mut v = ReceiptVerdict::skeleton("rcpt-001", true);
        v.trace_id = "trace-aaa".into();
        v.decision_id = "dec-bbb".into();
        v.policy_id = "pol-ccc".into();
        v.signature = LayerVerdict {
            passed: true,
            error_code: None,
            checks: vec![CheckEntry {
                check: "threshold_signature".into(),
                outcome: CheckOutcome::Pass,
                error_code: None,
                detail: "3/3 signers".into(),
            }],
        };
        v.transparency = LayerVerdict {
            passed: true,
            error_code: None,
            checks: vec![
                CheckEntry {
                    check: "mmr_inclusion".into(),
                    outcome: CheckOutcome::Pass,
                    error_code: None,
                    detail: "leaf 41 under root r9".into(),
                },
                CheckEntry {
                    check: "mmr_consistency".into(),
                    outcome: CheckOutcome::Pass,
                    error_code: None,
                    detail: "c0->c1 consistent".into(),
                },
            ],
        };
        v.attestation = LayerVerdict::default();
        v.attestation.passed = true;
        v.posterior_snapshot = Some(PosteriorSnapshot {
            point_estimate: Some(0.873),
            ci_lower: Some(0.81),
            ci_upper: Some(0.92),
        });
        v
    }

    fn make_attestation_fail() -> ReceiptVerdict {
        let mut v = ReceiptVerdict::skeleton("rcpt-001", false);
        v.trace_id = "trace-aaa".into();
        v.decision_id = "dec-bbb".into();
        v.policy_id = "pol-ccc".into();
        v.failure_class = Some(FailureClass::Attestation);
        v.exit_code = 2;
        v.signature = LayerVerdict {
            passed: true,
            error_code: None,
            checks: vec![],
        };
        v.transparency = LayerVerdict {
            passed: true,
            error_code: None,
            checks: vec![],
        };
        v.attestation = LayerVerdict {
            passed: false,
            error_code: Some("ATTEST_STALE".into()),
            checks: vec![CheckEntry {
                check: "quote_freshness".into(),
                outcome: CheckOutcome::Fail,
                error_code: Some("ATTEST_STALE".into()),
                detail: "quote older than max age".into(),
            }],
        };
        v.warnings = vec!["attestation degraded; running in safe-mode".into()];
        v
    }

    fn extract_row(frame: &Frame, y: u16, width: u16) -> String {
        let mut row = String::new();
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                row.push(ch);
            } else {
                row.push(' ');
            }
        }
        row
    }

    fn frame_has_text(frame: &Frame, width: u16, height: u16, needle: &str) -> bool {
        for y in 0..height {
            if extract_row(frame, y, width).contains(needle) {
                return true;
            }
        }
        false
    }

    #[test]
    fn check_outcome_parse_round_trip() {
        assert_eq!(CheckOutcome::parse("pass"), CheckOutcome::Pass);
        assert_eq!(CheckOutcome::parse("PASS"), CheckOutcome::Pass);
        assert_eq!(CheckOutcome::parse("fail"), CheckOutcome::Fail);
        assert_eq!(CheckOutcome::parse("FAIL"), CheckOutcome::Fail);
        assert_eq!(CheckOutcome::parse("anything-else"), CheckOutcome::Warn);
        assert_eq!(CheckOutcome::Pass.label(), "PASS");
        assert_eq!(CheckOutcome::Fail.label(), "FAIL");
        assert_eq!(CheckOutcome::Warn.label(), "WARN");
    }

    #[test]
    fn failure_class_parse_maps_all_known() {
        assert_eq!(
            FailureClass::parse("Signature"),
            Some(FailureClass::Signature)
        );
        assert_eq!(
            FailureClass::parse("Transparency"),
            Some(FailureClass::Transparency)
        );
        assert_eq!(
            FailureClass::parse("Attestation"),
            Some(FailureClass::Attestation)
        );
        assert_eq!(
            FailureClass::parse("StaleData"),
            Some(FailureClass::StaleData)
        );
        assert_eq!(FailureClass::parse("null"), None);
        assert_eq!(FailureClass::parse(""), None);
    }

    #[test]
    fn pass_verdict_renders_verified_and_provenance() {
        let v = make_pass();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 12, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 80, 12), &mut frame);
        assert!(frame_has_text(&frame, 80, 12, "VERIFIED"));
        assert!(frame_has_text(&frame, 80, 12, "receipt rcpt-001"));
        assert!(frame_has_text(&frame, 80, 12, "trace=trace-aaa"));
        assert!(frame_has_text(&frame, 80, 12, "signature"));
        assert!(frame_has_text(&frame, 80, 12, "transparency"));
        assert!(frame_has_text(&frame, 80, 12, "attestation"));
    }

    #[test]
    fn fail_verdict_surfaces_failure_class_in_verdict_row() {
        let v = make_attestation_fail();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 18, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 80, 18), &mut frame);
        assert!(frame_has_text(&frame, 80, 18, "FAILED"));
        assert!(frame_has_text(&frame, 80, 18, "failure_class=Attestation"));
        // attestation layer surfaces its error_code on the layer row.
        assert!(frame_has_text(&frame, 80, 18, "error_code=ATTEST_STALE"));
    }

    #[test]
    fn show_posterior_path_renders_point_estimate() {
        let v = make_pass();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 18, &mut pool);
        ReceiptVerifierPanel::new(&v)
            .show_posterior_path(true)
            .render(Rect::new(0, 0, 80, 18), &mut frame);
        assert!(frame_has_text(&frame, 80, 18, "posterior path:"));
        assert!(frame_has_text(&frame, 80, 18, "point_estimate = 0.873"));
        assert!(frame_has_text(
            &frame,
            80,
            18,
            "confidence_interval_95_lower = 0.81"
        ));
    }

    #[test]
    fn show_evidence_chain_renders_layer_checks() {
        let v = make_pass();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 18, &mut pool);
        ReceiptVerifierPanel::new(&v)
            .show_evidence_chain(true)
            .render(Rect::new(0, 0, 80, 18), &mut frame);
        assert!(frame_has_text(&frame, 80, 18, "evidence chain:"));
        assert!(frame_has_text(&frame, 80, 18, "mmr_inclusion"));
        assert!(frame_has_text(&frame, 80, 18, "mmr_consistency"));
        assert!(frame_has_text(&frame, 80, 18, "threshold_signature"));
    }

    #[test]
    fn attestation_failure_triage_mentions_safe_mode() {
        let v = make_attestation_fail();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 80, 24), &mut frame);
        assert!(frame_has_text(&frame, 80, 24, "incident triage"));
        assert!(frame_has_text(&frame, 80, 24, "SAFE-MODE"));
    }

    #[test]
    fn warnings_block_renders_when_present() {
        let v = make_attestation_fail();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 80, 24), &mut frame);
        assert!(frame_has_text(&frame, 80, 24, "warnings:"));
        assert!(frame_has_text(&frame, 80, 24, "running in safe-mode"));
    }

    #[test]
    fn empty_receipt_id_still_renders_provenance_dash() {
        let v = ReceiptVerdict::skeleton("rcpt-empty", true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 80, 10), &mut frame);
        // Empty trace/decision/policy → rendered as `-`.
        assert!(frame_has_text(&frame, 80, 10, "trace=-"));
        assert!(frame_has_text(&frame, 80, 10, "decision=-"));
        assert!(frame_has_text(&frame, 80, 10, "policy=-"));
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let v = make_pass();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 3, 2), &mut frame);
        let mut frame = Frame::new(1, 1, &mut pool);
        ReceiptVerifierPanel::new(&v).render(Rect::new(0, 0, 1, 1), &mut frame);
    }

    #[test]
    fn min_height_grows_with_options() {
        let v = make_pass();
        let base = ReceiptVerifierPanel::new(&v).min_height();
        let with_post = ReceiptVerifierPanel::new(&v)
            .show_posterior_path(true)
            .min_height();
        assert!(
            with_post > base,
            "posterior path should increase min_height"
        );
        let with_chain = ReceiptVerifierPanel::new(&v)
            .show_evidence_chain(true)
            .min_height();
        assert!(
            with_chain > base,
            "evidence chain should increase min_height"
        );
    }

    /// Selftest: the canonical verifier output JSON (lifted verbatim from
    /// `verify_receipt.sh selftest`) deserialises cleanly into [`ReceiptVerdict`]
    /// when the caller plumbs a serde_json pass through the data model.
    #[test]
    fn canonical_verifier_pass_json_round_trips_through_data_model() {
        let pass_json = r#"{
            "receipt_id":"rcpt-001","trace_id":"trace-aaa","decision_id":"dec-bbb",
            "policy_id":"pol-ccc","verification_timestamp_ns":1716000000000000000,
            "passed":true,"failure_class":null,"exit_code":0,
            "signature":{"passed":true,"error_code":null,"checks":[
                {"check":"threshold_signature","outcome":"pass","error_code":null,"detail":"3/3 signers"}
            ]},
            "transparency":{"passed":true,"error_code":null,"checks":[
                {"check":"mmr_inclusion","outcome":"pass","error_code":null,"detail":"leaf 41 under root r9"},
                {"check":"mmr_consistency","outcome":"pass","error_code":null,"detail":"c0->c1 consistent"}
            ]},
            "attestation":{"passed":true,"error_code":null,"checks":[]},
            "warnings":[],"logs":[]
        }"#;
        let v: serde_json::Value = serde_json::from_str(pass_json).unwrap();
        // Hand-marshal the JSON value into our data model — this also
        // doubles as the canonical reference adapter for callers.
        let verdict = verdict_from_json(&v).expect("canonical JSON should parse");
        assert_eq!(verdict.receipt_id, "rcpt-001");
        assert!(verdict.passed);
        assert_eq!(verdict.failure_class, None);
        assert_eq!(verdict.signature.checks.len(), 1);
        assert_eq!(verdict.transparency.checks.len(), 2);
        assert_eq!(verdict.transparency.checks[1].check, "mmr_consistency");
    }

    /// Test-only adapter from the verifier's JSON Value into [`ReceiptVerdict`].
    /// Kept in tests (not the public API) so the widget stays serde-free; this
    /// also doubles as a worked example for downstream callers.
    fn verdict_from_json(v: &serde_json::Value) -> Option<ReceiptVerdict> {
        fn layer(v: &serde_json::Value) -> LayerVerdict {
            let passed = v.get("passed").and_then(|x| x.as_bool()).unwrap_or(false);
            let error_code = v
                .get("error_code")
                .and_then(|x| x.as_str())
                .map(String::from);
            let checks = v
                .get("checks")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|c| CheckEntry {
                            check: c
                                .get("check")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            outcome: CheckOutcome::parse(
                                c.get("outcome").and_then(|x| x.as_str()).unwrap_or(""),
                            ),
                            error_code: c
                                .get("error_code")
                                .and_then(|x| x.as_str())
                                .map(String::from),
                            detail: c
                                .get("detail")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            LayerVerdict {
                passed,
                error_code,
                checks,
            }
        }
        Some(ReceiptVerdict {
            receipt_id: v.get("receipt_id")?.as_str()?.to_string(),
            trace_id: v
                .get("trace_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            decision_id: v
                .get("decision_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            policy_id: v
                .get("policy_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            verification_timestamp_ns: v
                .get("verification_timestamp_ns")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            passed: v.get("passed").and_then(|x| x.as_bool()).unwrap_or(false),
            failure_class: v
                .get("failure_class")
                .and_then(|x| x.as_str())
                .and_then(FailureClass::parse),
            exit_code: v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
            signature: v.get("signature").map(layer).unwrap_or_default(),
            transparency: v.get("transparency").map(layer).unwrap_or_default(),
            attestation: v.get("attestation").map(layer).unwrap_or_default(),
            warnings: v
                .get("warnings")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|w| w.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            posterior_snapshot: None,
        })
    }
}
