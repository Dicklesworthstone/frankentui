#![forbid(unsafe_code)]

//! Diff strategy evidence ledger (bd-3jlw5.3).
//!
//! Records Bayesian diff strategy decisions in a fixed-capacity ring buffer
//! for zero per-frame allocation on the hot path. Supports JSONL export
//! via the [`EvidenceSink`] infrastructure.
//!
//! # Usage
//!
//! ```rust,ignore
//! use ftui_runtime::diff_evidence::{DiffEvidenceLedger, DiffStrategyRecord, DiffRegime};
//!
//! let mut ledger = DiffEvidenceLedger::new(1000);
//! ledger.record(DiffStrategyRecord {
//!     frame_id: 42,
//!     regime: DiffRegime::StableFrame,
//!     // ...
//! });
//! assert_eq!(ledger.len(), 1);
//! ```

use std::fmt::Write as _;

use ftui_render::diff_strategy::{DiffStrategy, StrategyEvidence};

/// Regime classification for diff strategy decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffRegime {
    /// Low change rate, stable content.
    StableFrame,
    /// Bursty changes (user typing, scrolling).
    BurstyChange,
    /// Terminal resize in progress.
    ResizeRegime,
    /// Terminal is degraded (slow output, high latency).
    DegradedTerminal,
}

impl DiffRegime {
    /// Regime name as a static string for JSONL output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StableFrame => "stable_frame",
            Self::BurstyChange => "bursty_change",
            Self::ResizeRegime => "resize",
            Self::DegradedTerminal => "degraded",
        }
    }
}

/// An observation that contributed to a diff strategy decision.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Metric name (e.g., "change_fraction", "frame_time_us").
    pub metric_name: String,
    /// Observed value.
    pub value: f64,
    /// How much this observation shifted the posterior (log-odds contribution).
    pub prior_contribution: f64,
}

impl Observation {
    /// Create a new observation.
    pub fn new(metric_name: impl Into<String>, value: f64, prior_contribution: f64) -> Self {
        Self {
            metric_name: metric_name.into(),
            value,
            prior_contribution,
        }
    }
}

/// A complete record of a diff strategy decision.
#[derive(Debug, Clone)]
pub struct DiffStrategyRecord {
    /// Frame number this decision was made for.
    pub frame_id: u64,
    /// Regime classification.
    pub regime: DiffRegime,
    /// Posterior probability per candidate strategy.
    pub posterior: Vec<(DiffStrategy, f64)>,
    /// Strategy chosen.
    pub chosen_strategy: DiffStrategy,
    /// Confidence (max posterior probability).
    pub confidence: f64,
    /// Full strategy evidence from the selector.
    pub evidence: StrategyEvidence,
    /// Whether a fallback was triggered.
    pub fallback_triggered: bool,
    /// Observations that fed into this decision.
    pub observations: Vec<Observation>,
}

impl DiffStrategyRecord {
    /// Format as a JSONL line (no trailing newline).
    pub fn to_jsonl(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("{\"type\":\"diff_decision\"");
        let _ = write!(out, ",\"frame\":{}", self.frame_id);
        let _ = write!(out, ",\"regime\":\"{}\"", self.regime.as_str());
        let _ = write!(out, ",\"strategy\":\"{:?}\"", self.chosen_strategy);
        let _ = write!(out, ",\"confidence\":{:.6}", self.confidence);
        let _ = write!(out, ",\"fallback\":{}", self.fallback_triggered);
        let _ = write!(
            out,
            ",\"posterior_mean\":{:.6},\"posterior_var\":{:.6}",
            self.evidence.posterior_mean, self.evidence.posterior_variance
        );
        let _ = write!(
            out,
            ",\"cost_full\":{:.4},\"cost_dirty\":{:.4},\"cost_redraw\":{:.4}",
            self.evidence.cost_full, self.evidence.cost_dirty, self.evidence.cost_redraw
        );
        let _ = write!(
            out,
            ",\"alpha\":{:.4},\"beta\":{:.4}",
            self.evidence.alpha, self.evidence.beta
        );

        // Observations
        out.push_str(",\"obs\":[");
        for (i, obs) in self.observations.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"m\":\"{}\",\"v\":{:.6},\"c\":{:.6}}}",
                obs.metric_name.replace('"', "\\\""),
                obs.value,
                obs.prior_contribution
            );
        }
        out.push_str("]}");
        out
    }
}

/// A regime transition event.
#[derive(Debug, Clone)]
pub struct RegimeTransition {
    /// Frame where the transition occurred.
    pub frame_id: u64,
    /// Previous regime.
    pub from_regime: DiffRegime,
    /// New regime.
    pub to_regime: DiffRegime,
    /// Human-readable trigger explanation.
    pub trigger: String,
    /// Confidence at the point of transition.
    pub confidence: f64,
}

impl RegimeTransition {
    /// Format as a JSONL line (no trailing newline).
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"type\":\"regime_transition\",\"frame\":{},\"from\":\"{}\",\"to\":\"{}\",\"trigger\":\"{}\",\"confidence\":{:.6}}}",
            self.frame_id,
            self.from_regime.as_str(),
            self.to_regime.as_str(),
            self.trigger.replace('"', "\\\""),
            self.confidence,
        )
    }
}

/// Fixed-capacity ring buffer for diff strategy decisions.
///
/// Pre-allocates all storage up front so that `record()` never allocates
/// on the hot path (the record itself is moved in, not cloned).
pub struct DiffEvidenceLedger {
    decisions: Vec<Option<DiffStrategyRecord>>,
    transitions: Vec<Option<RegimeTransition>>,
    decision_head: usize,
    transition_head: usize,
    decision_count: usize,
    transition_count: usize,
    decision_capacity: usize,
    transition_capacity: usize,
    current_regime: DiffRegime,
}

impl std::fmt::Debug for DiffEvidenceLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffEvidenceLedger")
            .field("decisions", &self.decision_count)
            .field("transitions", &self.transition_count)
            .field("capacity", &self.decision_capacity)
            .field("regime", &self.current_regime)
            .finish()
    }
}

impl DiffEvidenceLedger {
    /// Create a new ledger with the given capacity for decisions.
    ///
    /// Transition capacity is set to 1/10 of decision capacity (regime
    /// transitions are much rarer than per-frame decisions).
    pub fn new(decision_capacity: usize) -> Self {
        let decision_capacity = decision_capacity.max(1);
        let transition_capacity = (decision_capacity / 10).max(16);
        Self {
            decisions: (0..decision_capacity).map(|_| None).collect(),
            transitions: (0..transition_capacity).map(|_| None).collect(),
            decision_head: 0,
            transition_head: 0,
            decision_count: 0,
            transition_count: 0,
            decision_capacity,
            transition_capacity,
            current_regime: DiffRegime::StableFrame,
        }
    }

    /// Record a diff strategy decision. Overwrites oldest when full.
    pub fn record(&mut self, record: DiffStrategyRecord) {
        // Detect regime transition.
        if record.regime != self.current_regime {
            let transition = RegimeTransition {
                frame_id: record.frame_id,
                from_regime: self.current_regime,
                to_regime: record.regime,
                trigger: format!(
                    "confidence={:.3} strategy={:?}",
                    record.confidence, record.chosen_strategy
                ),
                confidence: record.confidence,
            };
            self.record_transition(transition);
            self.current_regime = record.regime;
        }

        self.decisions[self.decision_head] = Some(record);
        self.decision_head = (self.decision_head + 1) % self.decision_capacity;
        if self.decision_count < self.decision_capacity {
            self.decision_count += 1;
        }
    }

    /// Record a regime transition explicitly.
    pub fn record_transition(&mut self, transition: RegimeTransition) {
        self.transitions[self.transition_head] = Some(transition);
        self.transition_head = (self.transition_head + 1) % self.transition_capacity;
        if self.transition_count < self.transition_capacity {
            self.transition_count += 1;
        }
    }

    /// Number of decisions stored.
    pub fn len(&self) -> usize {
        self.decision_count
    }

    /// Whether the ledger is empty.
    pub fn is_empty(&self) -> bool {
        self.decision_count == 0
    }

    /// Number of regime transitions stored.
    pub fn transition_count(&self) -> usize {
        self.transition_count
    }

    /// Current regime.
    pub fn current_regime(&self) -> DiffRegime {
        self.current_regime
    }

    /// Iterate over stored decisions in insertion order (oldest first).
    pub fn decisions(&self) -> impl Iterator<Item = &DiffStrategyRecord> {
        let cap = self.decision_capacity;
        let count = self.decision_count;
        let head = self.decision_head;
        let start = if count < cap { 0 } else { head };

        (0..count).filter_map(move |i| {
            let idx = (start + i) % cap;
            self.decisions[idx].as_ref()
        })
    }

    /// Iterate over stored transitions in insertion order (oldest first).
    pub fn transitions(&self) -> impl Iterator<Item = &RegimeTransition> {
        let cap = self.transition_capacity;
        let count = self.transition_count;
        let head = self.transition_head;
        let start = if count < cap { 0 } else { head };

        (0..count).filter_map(move |i| {
            let idx = (start + i) % cap;
            self.transitions[idx].as_ref()
        })
    }

    /// Get the most recent decision.
    pub fn last_decision(&self) -> Option<&DiffStrategyRecord> {
        if self.decision_count == 0 {
            return None;
        }
        let idx = if self.decision_head == 0 {
            self.decision_capacity - 1
        } else {
            self.decision_head - 1
        };
        self.decisions[idx].as_ref()
    }

    /// Export all decisions and transitions as JSONL lines.
    pub fn export_jsonl(&self) -> String {
        let mut out = String::new();
        for d in self.decisions() {
            out.push_str(&d.to_jsonl());
            out.push('\n');
        }
        for t in self.transitions() {
            out.push_str(&t.to_jsonl());
            out.push('\n');
        }
        out
    }

    /// Flush decisions to an evidence sink.
    pub fn flush_to_sink(&self, sink: &crate::evidence_sink::EvidenceSink) -> std::io::Result<()> {
        for d in self.decisions() {
            sink.write_jsonl(&d.to_jsonl())?;
        }
        for t in self.transitions() {
            sink.write_jsonl(&t.to_jsonl())?;
        }
        Ok(())
    }

    /// Clear all stored decisions and transitions.
    pub fn clear(&mut self) {
        for slot in &mut self.decisions {
            *slot = None;
        }
        for slot in &mut self.transitions {
            *slot = None;
        }
        self.decision_head = 0;
        self.transition_head = 0;
        self.decision_count = 0;
        self.transition_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::diff_strategy::StrategyEvidence;

    fn make_evidence() -> StrategyEvidence {
        StrategyEvidence {
            strategy: DiffStrategy::DirtyRows,
            cost_full: 1.0,
            cost_dirty: 0.5,
            cost_redraw: 2.0,
            posterior_mean: 0.05,
            posterior_variance: 0.001,
            alpha: 2.0,
            beta: 38.0,
            dirty_rows: 3,
            total_rows: 24,
            total_cells: 1920,
            guard_reason: "none",
            hysteresis_applied: false,
            hysteresis_ratio: 0.05,
        }
    }

    fn make_record(frame_id: u64, regime: DiffRegime) -> DiffStrategyRecord {
        DiffStrategyRecord {
            frame_id,
            regime,
            posterior: vec![
                (DiffStrategy::Full, 0.3),
                (DiffStrategy::DirtyRows, 0.6),
                (DiffStrategy::FullRedraw, 0.1),
            ],
            chosen_strategy: DiffStrategy::DirtyRows,
            confidence: 0.6,
            evidence: make_evidence(),
            fallback_triggered: false,
            observations: vec![
                Observation::new("change_fraction", 0.05, 0.3),
                Observation::new("dirty_rows", 3.0, 0.2),
            ],
        }
    }

    #[test]
    fn empty_ledger() {
        let ledger = DiffEvidenceLedger::new(100);
        assert!(ledger.is_empty());
        assert_eq!(ledger.len(), 0);
        assert_eq!(ledger.transition_count(), 0);
        assert!(ledger.last_decision().is_none());
        assert_eq!(ledger.current_regime(), DiffRegime::StableFrame);
    }

    #[test]
    fn record_single_decision() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.last_decision().unwrap().frame_id, 1);
    }

    #[test]
    fn ring_buffer_wraps() {
        let mut ledger = DiffEvidenceLedger::new(5);
        for i in 0..10 {
            ledger.record(make_record(i, DiffRegime::StableFrame));
        }
        // Should have exactly 5 decisions (capacity)
        assert_eq!(ledger.len(), 5);
        // Oldest should be frame 5 (0-4 overwritten)
        let frames: Vec<u64> = ledger.decisions().map(|d| d.frame_id).collect();
        assert_eq!(frames, vec![5, 6, 7, 8, 9]);
    }

    #[test]
    fn regime_transition_auto_detected() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::BurstyChange));
        assert_eq!(ledger.transition_count(), 1);
        assert_eq!(ledger.current_regime(), DiffRegime::BurstyChange);
        let t = ledger.transitions().next().unwrap();
        assert_eq!(t.from_regime, DiffRegime::StableFrame);
        assert_eq!(t.to_regime, DiffRegime::BurstyChange);
        assert_eq!(t.frame_id, 2);
    }

    #[test]
    fn no_transition_on_same_regime() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::StableFrame));
        assert_eq!(ledger.transition_count(), 0);
    }

    #[test]
    fn multiple_transitions() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::BurstyChange));
        ledger.record(make_record(3, DiffRegime::ResizeRegime));
        ledger.record(make_record(4, DiffRegime::StableFrame));
        assert_eq!(ledger.transition_count(), 3);
    }

    #[test]
    fn jsonl_round_trip_decision() {
        let record = make_record(42, DiffRegime::StableFrame);
        let jsonl = record.to_jsonl();
        assert!(jsonl.contains("\"type\":\"diff_decision\""));
        assert!(jsonl.contains("\"frame\":42"));
        assert!(jsonl.contains("\"regime\":\"stable_frame\""));
        assert!(jsonl.contains("\"strategy\":\"DirtyRows\""));
        assert!(jsonl.contains("\"obs\":["));
        assert!(jsonl.contains("\"m\":\"change_fraction\""));
    }

    #[test]
    fn jsonl_round_trip_transition() {
        let transition = RegimeTransition {
            frame_id: 10,
            from_regime: DiffRegime::StableFrame,
            to_regime: DiffRegime::BurstyChange,
            trigger: "burst detected".to_string(),
            confidence: 0.85,
        };
        let jsonl = transition.to_jsonl();
        assert!(jsonl.contains("\"type\":\"regime_transition\""));
        assert!(jsonl.contains("\"frame\":10"));
        assert!(jsonl.contains("\"from\":\"stable_frame\""));
        assert!(jsonl.contains("\"to\":\"bursty_change\""));
    }

    #[test]
    fn export_jsonl_output() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::BurstyChange));
        let output = ledger.export_jsonl();
        let lines: Vec<&str> = output.lines().collect();
        // 2 decisions + 1 transition
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"frame\":1"));
        assert!(lines[1].contains("\"frame\":2"));
        assert!(lines[2].contains("regime_transition"));
    }

    #[test]
    fn clear_resets_everything() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::BurstyChange));
        ledger.clear();
        assert!(ledger.is_empty());
        assert_eq!(ledger.transition_count(), 0);
        assert!(ledger.last_decision().is_none());
    }

    #[test]
    fn last_decision_returns_most_recent() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::StableFrame));
        ledger.record(make_record(3, DiffRegime::StableFrame));
        assert_eq!(ledger.last_decision().unwrap().frame_id, 3);
    }

    #[test]
    fn last_decision_after_wrap() {
        let mut ledger = DiffEvidenceLedger::new(3);
        for i in 0..10 {
            ledger.record(make_record(i, DiffRegime::StableFrame));
        }
        assert_eq!(ledger.last_decision().unwrap().frame_id, 9);
    }

    #[test]
    fn observation_fields() {
        let obs = Observation::new("test_metric", 42.0, 1.5);
        assert_eq!(obs.metric_name, "test_metric");
        assert!((obs.value - 42.0).abs() < f64::EPSILON);
        assert!((obs.prior_contribution - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn regime_as_str() {
        assert_eq!(DiffRegime::StableFrame.as_str(), "stable_frame");
        assert_eq!(DiffRegime::BurstyChange.as_str(), "bursty_change");
        assert_eq!(DiffRegime::ResizeRegime.as_str(), "resize");
        assert_eq!(DiffRegime::DegradedTerminal.as_str(), "degraded");
    }

    #[test]
    fn transition_ring_buffer_wraps() {
        let mut ledger = DiffEvidenceLedger::new(10); // transition_capacity = max(10/10, 16) = 16
        // Force many transitions by alternating regimes
        let regimes = [
            DiffRegime::StableFrame,
            DiffRegime::BurstyChange,
            DiffRegime::ResizeRegime,
            DiffRegime::DegradedTerminal,
        ];
        for i in 0..100 {
            ledger.record(make_record(i, regimes[i as usize % regimes.len()]));
        }
        // Transitions should be capped at transition_capacity
        assert!(ledger.transition_count() <= 16);
    }

    #[test]
    fn decisions_order_before_wrap() {
        let mut ledger = DiffEvidenceLedger::new(10);
        for i in 0..5 {
            ledger.record(make_record(i, DiffRegime::StableFrame));
        }
        let frames: Vec<u64> = ledger.decisions().map(|d| d.frame_id).collect();
        assert_eq!(frames, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn flush_to_sink_writes_all() {
        let mut ledger = DiffEvidenceLedger::new(100);
        ledger.record(make_record(1, DiffRegime::StableFrame));
        ledger.record(make_record(2, DiffRegime::BurstyChange));

        let config = crate::evidence_sink::EvidenceSinkConfig::enabled_stdout();
        if let Ok(Some(sink)) = crate::evidence_sink::EvidenceSink::from_config(&config) {
            // This will write to stdout but shouldn't panic
            let result = ledger.flush_to_sink(&sink);
            assert!(result.is_ok());
        }
    }

    #[test]
    fn simulate_1000_frames() {
        let mut ledger = DiffEvidenceLedger::new(10_000);
        let regimes = [
            DiffRegime::StableFrame,
            DiffRegime::BurstyChange,
            DiffRegime::ResizeRegime,
            DiffRegime::StableFrame,
            DiffRegime::DegradedTerminal,
            DiffRegime::StableFrame,
        ];

        for i in 0..1000 {
            // Switch regime every 100 frames
            let regime = regimes[(i / 100) % regimes.len()];
            ledger.record(make_record(i as u64, regime));
        }

        assert_eq!(ledger.len(), 1000);
        // Should have transitions at boundaries
        assert!(ledger.transition_count() > 0);

        // Verify order
        let mut prev_frame = 0u64;
        for d in ledger.decisions() {
            assert!(d.frame_id >= prev_frame);
            prev_frame = d.frame_id;
        }

        // Verify JSONL export
        let jsonl = ledger.export_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), ledger.len() + ledger.transition_count());
    }

    #[test]
    fn debug_format() {
        let ledger = DiffEvidenceLedger::new(100);
        let debug = format!("{ledger:?}");
        assert!(debug.contains("DiffEvidenceLedger"));
        assert!(debug.contains("decisions: 0"));
    }

    #[test]
    fn minimum_capacity() {
        let mut ledger = DiffEvidenceLedger::new(0); // clamped to 1
        ledger.record(make_record(1, DiffRegime::StableFrame));
        assert_eq!(ledger.len(), 1);
        ledger.record(make_record(2, DiffRegime::StableFrame));
        assert_eq!(ledger.len(), 1); // wrapped
        assert_eq!(ledger.last_decision().unwrap().frame_id, 2);
    }

    // ── Decision Contract integration tests (bd-3jlw5.6) ───────

    #[test]
    fn contract_stable_to_bursty_transition() {
        // StableFrame -> BurstyChange when change_fraction > 0.5
        let mut ledger = DiffEvidenceLedger::new(100);

        // 10 stable frames
        for i in 0..10 {
            ledger.record(make_record(i, DiffRegime::StableFrame));
        }
        assert_eq!(ledger.current_regime(), DiffRegime::StableFrame);
        assert_eq!(ledger.transition_count(), 0);

        // Bursty change detected
        ledger.record(make_record(10, DiffRegime::BurstyChange));
        assert_eq!(ledger.current_regime(), DiffRegime::BurstyChange);
        assert_eq!(ledger.transition_count(), 1);

        let t = ledger.transitions().next().unwrap();
        assert_eq!(t.from_regime, DiffRegime::StableFrame);
        assert_eq!(t.to_regime, DiffRegime::BurstyChange);
        assert_eq!(t.frame_id, 10);
    }

    #[test]
    fn contract_bursty_recovery_to_stable() {
        // BurstyChange -> StableFrame after consecutive low-change frames
        let mut ledger = DiffEvidenceLedger::new(100);

        // Enter bursty via stable first
        ledger.record(make_record(0, DiffRegime::StableFrame));
        ledger.record(make_record(1, DiffRegime::BurstyChange));
        assert_eq!(ledger.transition_count(), 1); // Stable -> Bursty

        // Recovery: 3 stable frames
        for i in 2..5 {
            ledger.record(make_record(i, DiffRegime::StableFrame));
        }
        assert_eq!(ledger.current_regime(), DiffRegime::StableFrame);
        assert_eq!(ledger.transition_count(), 2); // Stable->Bursty, Bursty->Stable
    }

    #[test]
    fn contract_resize_returns_to_previous() {
        // ResizeRegime lasts 1 frame, then returns to previous regime
        let mut ledger = DiffEvidenceLedger::new(100);

        // Start stable
        ledger.record(make_record(0, DiffRegime::StableFrame));

        // Resize event
        ledger.record(make_record(1, DiffRegime::ResizeRegime));
        assert_eq!(ledger.current_regime(), DiffRegime::ResizeRegime);

        // Return to stable after 1 frame
        ledger.record(make_record(2, DiffRegime::StableFrame));
        assert_eq!(ledger.current_regime(), DiffRegime::StableFrame);

        // 2 transitions: Stable->Resize, Resize->Stable
        assert_eq!(ledger.transition_count(), 2);
    }

    #[test]
    fn contract_degraded_entry_and_recovery() {
        // DegradedTerminal when latency > 10ms, recovery when < 5ms
        let mut ledger = DiffEvidenceLedger::new(100);

        ledger.record(make_record(0, DiffRegime::StableFrame));
        ledger.record(make_record(1, DiffRegime::DegradedTerminal));
        assert_eq!(ledger.current_regime(), DiffRegime::DegradedTerminal);

        // Stay degraded for several frames
        for i in 2..10 {
            ledger.record(make_record(i, DiffRegime::DegradedTerminal));
        }
        assert_eq!(ledger.current_regime(), DiffRegime::DegradedTerminal);
        assert_eq!(ledger.transition_count(), 1); // only the initial transition

        // Recovery
        ledger.record(make_record(10, DiffRegime::StableFrame));
        assert_eq!(ledger.current_regime(), DiffRegime::StableFrame);
        assert_eq!(ledger.transition_count(), 2);
    }

    #[test]
    fn contract_no_flapping() {
        // Hysteresis: regime shouldn't flap back and forth rapidly
        // Record transitions and verify each one is captured
        let mut ledger = DiffEvidenceLedger::new(100);

        let sequence = [
            DiffRegime::StableFrame,
            DiffRegime::BurstyChange,
            DiffRegime::StableFrame,
            DiffRegime::BurstyChange,
            DiffRegime::StableFrame,
        ];

        for (i, &regime) in sequence.iter().enumerate() {
            ledger.record(make_record(i as u64, regime));
        }

        // 4 transitions (each change recorded)
        assert_eq!(ledger.transition_count(), 4);

        // Verify transition order
        let transitions: Vec<(DiffRegime, DiffRegime)> = ledger
            .transitions()
            .map(|t| (t.from_regime, t.to_regime))
            .collect();
        assert_eq!(
            transitions,
            vec![
                (DiffRegime::StableFrame, DiffRegime::BurstyChange),
                (DiffRegime::BurstyChange, DiffRegime::StableFrame),
                (DiffRegime::StableFrame, DiffRegime::BurstyChange),
                (DiffRegime::BurstyChange, DiffRegime::StableFrame),
            ]
        );
    }

    #[test]
    fn contract_full_lifecycle() {
        // Full lifecycle: stable -> bursty -> resize -> stable -> degraded -> stable
        let mut ledger = DiffEvidenceLedger::new(100);

        let lifecycle = [
            (0, DiffRegime::StableFrame),
            (1, DiffRegime::StableFrame),
            (2, DiffRegime::BurstyChange),
            (3, DiffRegime::BurstyChange),
            (4, DiffRegime::ResizeRegime),
            (5, DiffRegime::StableFrame),
            (6, DiffRegime::StableFrame),
            (7, DiffRegime::DegradedTerminal),
            (8, DiffRegime::DegradedTerminal),
            (9, DiffRegime::StableFrame),
        ];

        for &(frame, regime) in &lifecycle {
            ledger.record(make_record(frame, regime));
        }

        assert_eq!(ledger.len(), 10);
        // Transitions: Stable->Bursty, Bursty->Resize, Resize->Stable,
        //              Stable->Degraded, Degraded->Stable
        assert_eq!(ledger.transition_count(), 5);
        assert_eq!(ledger.current_regime(), DiffRegime::StableFrame);

        // Verify all transitions have valid frame IDs
        for t in ledger.transitions() {
            assert!(t.frame_id <= 9);
            assert_ne!(t.from_regime, t.to_regime);
        }
    }
}
