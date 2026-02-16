#![forbid(unsafe_code)]

//! Unified Evidence Ledger for all Bayesian decision points (bd-fp38v).
//!
//! Every adaptive controller in FrankenTUI (diff strategy, resize coalescing,
//! frame budget, degradation, VOI sampling, hint ranking, command palette
//! scoring) emits decisions through this common schema. Each decision records:
//!
//! - `log_posterior`: log-odds of the chosen action being optimal
//! - Top-3 evidence terms with Bayes factors
//! - Action chosen
//! - Loss avoided (expected loss of next-best minus chosen)
//! - Confidence interval `[lower, upper]`
//!
//! The ledger is a fixed-capacity ring buffer (zero per-decision allocation on
//! the hot path). JSONL export is supported via [`EvidenceSink`].
//!
//! # Usage
//!
//! ```rust
//! use ftui_runtime::unified_evidence::{
//!     DecisionDomain, EvidenceEntry, EvidenceTerm, UnifiedEvidenceLedger,
//! };
//!
//! let mut ledger = UnifiedEvidenceLedger::new(1000);
//!
//! let entry = EvidenceEntry {
//!     decision_id: 1,
//!     timestamp_ns: 42_000,
//!     domain: DecisionDomain::DiffStrategy,
//!     log_posterior: 1.386,
//!     top_evidence: [
//!         Some(EvidenceTerm::new("change_rate", 4.0)),
//!         Some(EvidenceTerm::new("dirty_rows", 2.5)),
//!         None,
//!     ],
//!     action: "dirty_rows",
//!     loss_avoided: 0.15,
//!     confidence_interval: (0.72, 0.95),
//! };
//!
//! ledger.record(entry);
//! assert_eq!(ledger.len(), 1);
//! ```

use std::fmt::Write as _;

// ============================================================================
// Domain Enum
// ============================================================================

/// Domain of a Bayesian decision point.
///
/// Covers all 7 adaptive controllers in FrankenTUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionDomain {
    /// Diff strategy selection (full vs dirty-rows vs full-redraw).
    DiffStrategy,
    /// Resize event coalescing (apply vs coalesce vs placeholder).
    ResizeCoalescing,
    /// Frame budget allocation and PID-based timing.
    FrameBudget,
    /// Graceful degradation level selection.
    Degradation,
    /// Value-of-information adaptive sampling.
    VoiSampling,
    /// Hint ranking for type-ahead suggestions.
    HintRanking,
    /// Command palette relevance scoring.
    PaletteScoring,
}

impl DecisionDomain {
    /// Domain name as a static string for JSONL output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiffStrategy => "diff_strategy",
            Self::ResizeCoalescing => "resize_coalescing",
            Self::FrameBudget => "frame_budget",
            Self::Degradation => "degradation",
            Self::VoiSampling => "voi_sampling",
            Self::HintRanking => "hint_ranking",
            Self::PaletteScoring => "palette_scoring",
        }
    }

    /// All domains in declaration order.
    pub const ALL: [Self; 7] = [
        Self::DiffStrategy,
        Self::ResizeCoalescing,
        Self::FrameBudget,
        Self::Degradation,
        Self::VoiSampling,
        Self::HintRanking,
        Self::PaletteScoring,
    ];
}

// ============================================================================
// Evidence Term
// ============================================================================

/// A single piece of evidence contributing to a Bayesian decision.
///
/// Bayes factor > 1 supports the chosen action; < 1 opposes it.
#[derive(Debug, Clone)]
pub struct EvidenceTerm {
    /// Human-readable label (e.g., "change_rate", "word_boundary").
    pub label: &'static str,
    /// Bayes factor: `P(evidence | H1) / P(evidence | H0)`.
    pub bayes_factor: f64,
}

impl EvidenceTerm {
    /// Create a new evidence term.
    #[must_use]
    pub const fn new(label: &'static str, bayes_factor: f64) -> Self {
        Self {
            label,
            bayes_factor,
        }
    }

    /// Log Bayes factor (natural log).
    #[must_use]
    pub fn log_bf(&self) -> f64 {
        self.bayes_factor.ln()
    }
}

// ============================================================================
// Evidence Entry
// ============================================================================

/// Unified evidence record for any Bayesian decision point.
///
/// Fixed-size: the top-3 evidence array avoids heap allocation.
#[derive(Debug, Clone)]
pub struct EvidenceEntry {
    /// Monotonic decision counter (unique within a session).
    pub decision_id: u64,
    /// Monotonic timestamp (nanoseconds from program start).
    pub timestamp_ns: u64,
    /// Which decision domain this belongs to.
    pub domain: DecisionDomain,
    /// Log-posterior odds of the chosen action being optimal.
    pub log_posterior: f64,
    /// Top-3 evidence terms ranked by |log(BF)|, pre-allocated.
    pub top_evidence: [Option<EvidenceTerm>; 3],
    /// Action taken (e.g., "dirty_rows", "coalesce", "degrade_1").
    pub action: &'static str,
    /// Expected loss avoided: `E[loss(next_best)] - E[loss(chosen)]`.
    /// Non-negative when the chosen action is optimal.
    pub loss_avoided: f64,
    /// Confidence interval `(lower, upper)` on the posterior probability.
    pub confidence_interval: (f64, f64),
}

impl EvidenceEntry {
    /// Posterior probability derived from log-odds.
    #[must_use]
    pub fn posterior_probability(&self) -> f64 {
        let odds = self.log_posterior.exp();
        odds / (1.0 + odds)
    }

    /// Number of evidence terms present.
    #[must_use]
    pub fn evidence_count(&self) -> usize {
        self.top_evidence.iter().filter(|t| t.is_some()).count()
    }

    /// Combined log Bayes factor (sum of individual log-BFs).
    #[must_use]
    pub fn combined_log_bf(&self) -> f64 {
        self.top_evidence
            .iter()
            .filter_map(|t| t.as_ref())
            .map(|t| t.log_bf())
            .sum()
    }

    /// Format as a JSONL line (no trailing newline).
    pub fn to_jsonl(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("{\"schema\":\"ftui-evidence-v2\"");
        let _ = write!(out, ",\"id\":{}", self.decision_id);
        let _ = write!(out, ",\"ts_ns\":{}", self.timestamp_ns);
        let _ = write!(out, ",\"domain\":\"{}\"", self.domain.as_str());
        let _ = write!(out, ",\"log_posterior\":{:.6}", self.log_posterior);

        out.push_str(",\"evidence\":[");
        let mut first = true;
        for term in self.top_evidence.iter().flatten() {
            if !first {
                out.push(',');
            }
            first = false;
            let _ = write!(
                out,
                "{{\"label\":\"{}\",\"bf\":{:.6}}}",
                term.label, term.bayes_factor
            );
        }
        out.push(']');

        let _ = write!(out, ",\"action\":\"{}\"", self.action);
        let _ = write!(out, ",\"loss_avoided\":{:.6}", self.loss_avoided);
        let _ = write!(
            out,
            ",\"ci\":[{:.6},{:.6}]",
            self.confidence_interval.0, self.confidence_interval.1
        );
        out.push('}');
        out
    }
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for constructing `EvidenceEntry` values.
///
/// Handles automatic selection of top-3 evidence terms by |log(BF)|.
pub struct EvidenceEntryBuilder {
    decision_id: u64,
    timestamp_ns: u64,
    domain: DecisionDomain,
    log_posterior: f64,
    evidence: Vec<EvidenceTerm>,
    action: &'static str,
    loss_avoided: f64,
    confidence_interval: (f64, f64),
}

impl EvidenceEntryBuilder {
    /// Start building an evidence entry.
    pub fn new(domain: DecisionDomain, decision_id: u64, timestamp_ns: u64) -> Self {
        Self {
            decision_id,
            timestamp_ns,
            domain,
            log_posterior: 0.0,
            evidence: Vec::new(),
            action: "",
            loss_avoided: 0.0,
            confidence_interval: (0.0, 1.0),
        }
    }

    /// Set the log-posterior odds.
    #[must_use]
    pub fn log_posterior(mut self, value: f64) -> Self {
        self.log_posterior = value;
        self
    }

    /// Add an evidence term.
    #[must_use]
    pub fn evidence(mut self, label: &'static str, bayes_factor: f64) -> Self {
        self.evidence.push(EvidenceTerm::new(label, bayes_factor));
        self
    }

    /// Set the chosen action.
    #[must_use]
    pub fn action(mut self, action: &'static str) -> Self {
        self.action = action;
        self
    }

    /// Set the loss avoided.
    #[must_use]
    pub fn loss_avoided(mut self, value: f64) -> Self {
        self.loss_avoided = value;
        self
    }

    /// Set the confidence interval.
    #[must_use]
    pub fn confidence_interval(mut self, lower: f64, upper: f64) -> Self {
        self.confidence_interval = (lower, upper);
        self
    }

    /// Build the entry, selecting top-3 evidence terms by |log(BF)|.
    pub fn build(mut self) -> EvidenceEntry {
        // Sort by descending |log(BF)| to pick the top 3.
        self.evidence
            .sort_by(|a, b| b.log_bf().abs().total_cmp(&a.log_bf().abs()));

        let mut top = [None, None, None];
        for (i, term) in self.evidence.into_iter().take(3).enumerate() {
            top[i] = Some(term);
        }

        EvidenceEntry {
            decision_id: self.decision_id,
            timestamp_ns: self.timestamp_ns,
            domain: self.domain,
            log_posterior: self.log_posterior,
            top_evidence: top,
            action: self.action,
            loss_avoided: self.loss_avoided,
            confidence_interval: self.confidence_interval,
        }
    }
}

// ============================================================================
// Unified Evidence Ledger
// ============================================================================

/// Fixed-capacity ring buffer storing [`EvidenceEntry`] records from all
/// decision domains.
///
/// Pre-allocates all storage so that [`record`](Self::record) never
/// allocates on the hot path.
pub struct UnifiedEvidenceLedger {
    entries: Vec<Option<EvidenceEntry>>,
    head: usize,
    count: usize,
    capacity: usize,
    next_id: u64,
    /// Per-domain counters for audit and replay.
    domain_counts: [u64; 7],
}

impl std::fmt::Debug for UnifiedEvidenceLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnifiedEvidenceLedger")
            .field("count", &self.count)
            .field("capacity", &self.capacity)
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl UnifiedEvidenceLedger {
    /// Create a new ledger with the given capacity.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            entries: (0..capacity).map(|_| None).collect(),
            head: 0,
            count: 0,
            capacity,
            next_id: 0,
            domain_counts: [0; 7],
        }
    }

    /// Record an evidence entry. Overwrites oldest when full.
    ///
    /// Returns the assigned `decision_id`.
    pub fn record(&mut self, mut entry: EvidenceEntry) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        entry.decision_id = id;

        let domain_idx = entry.domain as usize;
        self.domain_counts[domain_idx] += 1;

        self.entries[self.head] = Some(entry);
        self.head = (self.head + 1) % self.capacity;
        if self.count < self.capacity {
            self.count += 1;
        }
        id
    }

    /// Number of entries currently stored.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the ledger is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Total entries ever recorded (including overwritten).
    pub fn total_recorded(&self) -> u64 {
        self.next_id
    }

    /// Number of decisions recorded for a specific domain.
    pub fn domain_count(&self, domain: DecisionDomain) -> u64 {
        self.domain_counts[domain as usize]
    }

    /// Iterate over stored entries in insertion order (oldest first).
    pub fn entries(&self) -> impl Iterator<Item = &EvidenceEntry> {
        let cap = self.capacity;
        let count = self.count;
        let head = self.head;
        let start = if count < cap { 0 } else { head };

        (0..count).filter_map(move |i| {
            let idx = (start + i) % cap;
            self.entries[idx].as_ref()
        })
    }

    /// Get entries for a specific domain.
    pub fn entries_for_domain(
        &self,
        domain: DecisionDomain,
    ) -> impl Iterator<Item = &EvidenceEntry> {
        self.entries().filter(move |e| e.domain == domain)
    }

    /// Get the most recent entry.
    pub fn last_entry(&self) -> Option<&EvidenceEntry> {
        if self.count == 0 {
            return None;
        }
        let idx = if self.head == 0 {
            self.capacity - 1
        } else {
            self.head - 1
        };
        self.entries[idx].as_ref()
    }

    /// Get the most recent entry for a specific domain.
    pub fn last_entry_for_domain(&self, domain: DecisionDomain) -> Option<&EvidenceEntry> {
        // Walk backwards from head.
        let start = if self.head == 0 {
            self.capacity - 1
        } else {
            self.head - 1
        };
        for i in 0..self.count {
            let idx = (start + self.capacity - i) % self.capacity;
            if let Some(entry) = &self.entries[idx]
                && entry.domain == domain
            {
                return Some(entry);
            }
        }
        None
    }

    /// Export all entries as JSONL.
    pub fn export_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in self.entries() {
            out.push_str(&entry.to_jsonl());
            out.push('\n');
        }
        out
    }

    /// Flush entries to an evidence sink.
    pub fn flush_to_sink(&self, sink: &crate::evidence_sink::EvidenceSink) -> std::io::Result<()> {
        for entry in self.entries() {
            sink.write_jsonl(&entry.to_jsonl())?;
        }
        Ok(())
    }

    /// Clear all stored entries. Domain counters are preserved.
    pub fn clear(&mut self) {
        for slot in &mut self.entries {
            *slot = None;
        }
        self.head = 0;
        self.count = 0;
    }

    /// Summary statistics per domain.
    pub fn summary(&self) -> LedgerSummary {
        let mut per_domain = [(0u64, 0.0f64, 0.0f64); 7]; // (count, sum_loss, sum_posterior)
        for entry in self.entries() {
            let idx = entry.domain as usize;
            per_domain[idx].0 += 1;
            per_domain[idx].1 += entry.loss_avoided;
            per_domain[idx].2 += entry.posterior_probability();
        }

        let domains: Vec<DomainSummary> = DecisionDomain::ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| per_domain[*i].0 > 0)
            .map(|(i, domain)| {
                let (count, sum_loss, sum_posterior) = per_domain[i];
                DomainSummary {
                    domain: *domain,
                    decision_count: count,
                    mean_loss_avoided: sum_loss / count as f64,
                    mean_posterior: sum_posterior / count as f64,
                }
            })
            .collect();

        LedgerSummary {
            total_decisions: self.next_id,
            stored_decisions: self.count as u64,
            domains,
        }
    }
}

/// Summary of ledger contents.
#[derive(Debug, Clone)]
pub struct LedgerSummary {
    /// Total decisions ever recorded.
    pub total_decisions: u64,
    /// Decisions currently stored in the ring buffer.
    pub stored_decisions: u64,
    /// Per-domain statistics.
    pub domains: Vec<DomainSummary>,
}

/// Per-domain summary statistics.
#[derive(Debug, Clone)]
pub struct DomainSummary {
    /// Decision domain.
    pub domain: DecisionDomain,
    /// Number of decisions from this domain in the buffer.
    pub decision_count: u64,
    /// Mean loss avoided across decisions.
    pub mean_loss_avoided: f64,
    /// Mean posterior probability across decisions.
    pub mean_posterior: f64,
}

// ============================================================================
// Trait: EmitsEvidence
// ============================================================================

/// Trait for decision-making components that emit unified evidence.
///
/// Implement this on each Bayesian controller to bridge its domain-specific
/// evidence into the unified schema.
pub trait EmitsEvidence {
    /// Convert the current decision state into a unified evidence entry.
    fn to_evidence_entry(&self, timestamp_ns: u64) -> EvidenceEntry;

    /// The decision domain this component belongs to.
    fn evidence_domain(&self) -> DecisionDomain;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(domain: DecisionDomain, action: &'static str) -> EvidenceEntry {
        EvidenceEntry {
            decision_id: 0, // assigned by ledger
            timestamp_ns: 1_000_000,
            domain,
            log_posterior: 1.386, // ~80% posterior
            top_evidence: [
                Some(EvidenceTerm::new("change_rate", 4.0)),
                Some(EvidenceTerm::new("dirty_rows", 2.5)),
                None,
            ],
            action,
            loss_avoided: 0.15,
            confidence_interval: (0.72, 0.95),
        }
    }

    #[test]
    fn empty_ledger() {
        let ledger = UnifiedEvidenceLedger::new(100);
        assert!(ledger.is_empty());
        assert_eq!(ledger.len(), 0);
        assert_eq!(ledger.total_recorded(), 0);
        assert!(ledger.last_entry().is_none());
    }

    #[test]
    fn record_single() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        let id = ledger.record(make_entry(DecisionDomain::DiffStrategy, "dirty_rows"));
        assert_eq!(id, 0);
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.total_recorded(), 1);
        assert_eq!(ledger.last_entry().unwrap().action, "dirty_rows");
    }

    #[test]
    fn record_multiple_domains() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "dirty_rows"));
        ledger.record(make_entry(DecisionDomain::ResizeCoalescing, "coalesce"));
        ledger.record(make_entry(DecisionDomain::HintRanking, "rank_3"));

        assert_eq!(ledger.len(), 3);
        assert_eq!(ledger.domain_count(DecisionDomain::DiffStrategy), 1);
        assert_eq!(ledger.domain_count(DecisionDomain::ResizeCoalescing), 1);
        assert_eq!(ledger.domain_count(DecisionDomain::HintRanking), 1);
        assert_eq!(ledger.domain_count(DecisionDomain::FrameBudget), 0);
    }

    #[test]
    fn ring_buffer_wraps() {
        let mut ledger = UnifiedEvidenceLedger::new(5);
        for i in 0..10u64 {
            let mut e = make_entry(DecisionDomain::DiffStrategy, "full");
            e.timestamp_ns = i * 1000;
            ledger.record(e);
        }
        assert_eq!(ledger.len(), 5);
        assert_eq!(ledger.total_recorded(), 10);

        let ids: Vec<u64> = ledger.entries().map(|e| e.decision_id).collect();
        assert_eq!(ids, vec![5, 6, 7, 8, 9]);
    }

    #[test]
    fn entries_for_domain() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        ledger.record(make_entry(DecisionDomain::ResizeCoalescing, "apply"));
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "dirty_rows"));

        let diff_entries: Vec<&str> = ledger
            .entries_for_domain(DecisionDomain::DiffStrategy)
            .map(|e| e.action)
            .collect();
        assert_eq!(diff_entries, vec!["full", "dirty_rows"]);
    }

    #[test]
    fn last_entry_for_domain() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        ledger.record(make_entry(DecisionDomain::ResizeCoalescing, "apply"));
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "dirty_rows"));

        let last = ledger
            .last_entry_for_domain(DecisionDomain::DiffStrategy)
            .unwrap();
        assert_eq!(last.action, "dirty_rows");

        let last_resize = ledger
            .last_entry_for_domain(DecisionDomain::ResizeCoalescing)
            .unwrap();
        assert_eq!(last_resize.action, "apply");

        assert!(
            ledger
                .last_entry_for_domain(DecisionDomain::FrameBudget)
                .is_none()
        );
    }

    #[test]
    fn posterior_probability() {
        let entry = make_entry(DecisionDomain::DiffStrategy, "full");
        let prob = entry.posterior_probability();
        // log_posterior = 1.386 → odds = e^1.386 ≈ 4.0 → prob ≈ 0.8
        assert!((prob - 0.8).abs() < 0.01);
    }

    #[test]
    fn evidence_count() {
        let entry = make_entry(DecisionDomain::DiffStrategy, "full");
        assert_eq!(entry.evidence_count(), 2); // two Some, one None
    }

    #[test]
    fn combined_log_bf() {
        let entry = make_entry(DecisionDomain::DiffStrategy, "full");
        let expected = 4.0f64.ln() + 2.5f64.ln();
        assert!((entry.combined_log_bf() - expected).abs() < 1e-10);
    }

    #[test]
    fn jsonl_output() {
        let entry = make_entry(DecisionDomain::DiffStrategy, "dirty_rows");
        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\"schema\":\"ftui-evidence-v2\""));
        assert!(jsonl.contains("\"domain\":\"diff_strategy\""));
        assert!(jsonl.contains("\"action\":\"dirty_rows\""));
        assert!(jsonl.contains("\"change_rate\""));
        assert!(jsonl.contains("\"bf\":4.0"));
        assert!(jsonl.contains("\"ci\":["));
        // Verify it's valid single-line JSON (no newlines).
        assert!(!jsonl.contains('\n'));
    }

    #[test]
    fn export_jsonl() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        ledger.record(make_entry(DecisionDomain::ResizeCoalescing, "apply"));
        let output = ledger.export_jsonl();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("diff_strategy"));
        assert!(lines[1].contains("resize_coalescing"));
    }

    #[test]
    fn clear() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "dirty_rows"));
        ledger.clear();
        assert!(ledger.is_empty());
        assert_eq!(ledger.total_recorded(), 2); // total preserved
        assert!(ledger.last_entry().is_none());
    }

    #[test]
    fn summary() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        for _ in 0..5 {
            ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        }
        for _ in 0..3 {
            ledger.record(make_entry(DecisionDomain::HintRanking, "rank_1"));
        }

        let summary = ledger.summary();
        assert_eq!(summary.total_decisions, 8);
        assert_eq!(summary.stored_decisions, 8);
        assert_eq!(summary.domains.len(), 2);

        let diff = summary
            .domains
            .iter()
            .find(|d| d.domain == DecisionDomain::DiffStrategy)
            .unwrap();
        assert_eq!(diff.decision_count, 5);
        assert!(diff.mean_posterior > 0.0);
    }

    #[test]
    fn builder_selects_top_3() {
        let entry = EvidenceEntryBuilder::new(DecisionDomain::PaletteScoring, 0, 1000)
            .log_posterior(2.0)
            .evidence("match_type", 9.0) // log(9) = 2.197
            .evidence("position", 1.5) // log(1.5) = 0.405
            .evidence("word_boundary", 2.0) // log(2) = 0.693
            .evidence("gap_penalty", 0.5) // log(0.5) = -0.693 (abs = 0.693)
            .evidence("tag_match", 3.0) // log(3) = 1.099
            .action("exact")
            .loss_avoided(0.8)
            .confidence_interval(0.90, 0.99)
            .build();

        // Top 3 by |log(BF)|: match_type (2.197), tag_match (1.099),
        // then word_boundary or gap_penalty (both 0.693 abs).
        assert_eq!(entry.evidence_count(), 3);
        assert_eq!(entry.top_evidence[0].as_ref().unwrap().label, "match_type");
        assert_eq!(entry.top_evidence[1].as_ref().unwrap().label, "tag_match");
        // Third is either word_boundary or gap_penalty (same |log(BF)|).
        let third = entry.top_evidence[2].as_ref().unwrap().label;
        assert!(
            third == "word_boundary" || third == "gap_penalty",
            "unexpected third: {third}"
        );
    }

    #[test]
    fn builder_fewer_than_3() {
        let entry = EvidenceEntryBuilder::new(DecisionDomain::FrameBudget, 0, 1000)
            .evidence("frame_time", 2.0)
            .action("hold")
            .build();

        assert_eq!(entry.evidence_count(), 1);
        assert!(entry.top_evidence[1].is_none());
        assert!(entry.top_evidence[2].is_none());
    }

    #[test]
    fn domain_all_covers_seven() {
        assert_eq!(DecisionDomain::ALL.len(), 7);
    }

    #[test]
    fn domain_as_str_roundtrip() {
        for domain in DecisionDomain::ALL {
            let s = domain.as_str();
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }

    #[test]
    fn minimum_capacity() {
        let mut ledger = UnifiedEvidenceLedger::new(0); // clamped to 1
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        assert_eq!(ledger.len(), 1);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "dirty_rows"));
        assert_eq!(ledger.len(), 1); // wrapped
        assert_eq!(ledger.last_entry().unwrap().action, "dirty_rows");
    }

    #[test]
    fn debug_format() {
        let ledger = UnifiedEvidenceLedger::new(100);
        let debug = format!("{ledger:?}");
        assert!(debug.contains("UnifiedEvidenceLedger"));
        assert!(debug.contains("count: 0"));
    }

    #[test]
    fn entries_order_before_wrap() {
        let mut ledger = UnifiedEvidenceLedger::new(10);
        for i in 0..5u64 {
            let mut e = make_entry(DecisionDomain::DiffStrategy, "full");
            e.timestamp_ns = i;
            ledger.record(e);
        }
        let ids: Vec<u64> = ledger.entries().map(|e| e.decision_id).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn evidence_term_log_bf() {
        let term = EvidenceTerm::new("test", 4.0);
        assert!((term.log_bf() - 4.0f64.ln()).abs() < 1e-10);
    }

    #[test]
    fn loss_avoided_nonnegative_for_optimal() {
        let entry = make_entry(DecisionDomain::DiffStrategy, "full");
        assert!(entry.loss_avoided >= 0.0);
    }

    #[test]
    fn confidence_interval_bounds() {
        let entry = make_entry(DecisionDomain::DiffStrategy, "full");
        assert!(entry.confidence_interval.0 <= entry.confidence_interval.1);
        assert!(entry.confidence_interval.0 >= 0.0);
        assert!(entry.confidence_interval.1 <= 1.0);
    }

    #[test]
    fn flush_to_sink_writes_all() {
        let mut ledger = UnifiedEvidenceLedger::new(100);
        ledger.record(make_entry(DecisionDomain::DiffStrategy, "full"));
        ledger.record(make_entry(DecisionDomain::HintRanking, "rank_1"));

        let config = crate::evidence_sink::EvidenceSinkConfig::enabled_stdout();
        if let Ok(Some(sink)) = crate::evidence_sink::EvidenceSink::from_config(&config) {
            let result = ledger.flush_to_sink(&sink);
            assert!(result.is_ok());
        }
    }

    #[test]
    fn simulate_mixed_domains() {
        let mut ledger = UnifiedEvidenceLedger::new(10_000);
        let domains = DecisionDomain::ALL;
        let actions = [
            "full",
            "coalesce",
            "hold",
            "degrade_1",
            "sample",
            "rank_1",
            "exact",
        ];

        for i in 0..1000u64 {
            let domain = domains[(i as usize) % 7];
            let action = actions[(i as usize) % 7];
            let mut e = make_entry(domain, action);
            e.timestamp_ns = i * 16_000; // ~16ms per frame
            ledger.record(e);
        }

        assert_eq!(ledger.len(), 1000);
        assert_eq!(ledger.total_recorded(), 1000);

        // Each domain should have ~142-143 entries.
        for domain in DecisionDomain::ALL {
            let count = ledger.domain_count(domain);
            assert!(
                (142..=143).contains(&count),
                "{:?}: expected ~142, got {}",
                domain,
                count
            );
        }

        // JSONL export should produce 1000 lines.
        let jsonl = ledger.export_jsonl();
        assert_eq!(jsonl.lines().count(), 1000);
    }

    // ── bd-xox.10: Serialization and Schema Tests ─────────────────────────

    #[test]
    fn jsonl_roundtrip_all_fields() {
        let entry = EvidenceEntryBuilder::new(DecisionDomain::DiffStrategy, 42, 999_000)
            .log_posterior(1.386)
            .evidence("change_rate", 4.0)
            .evidence("dirty_ratio", 2.5)
            .action("dirty_rows")
            .loss_avoided(0.15)
            .confidence_interval(0.72, 0.95)
            .build();

        let jsonl = entry.to_jsonl();
        let parsed: serde_json::Value = serde_json::from_str(&jsonl).expect("valid JSON");

        // Verify every required field is present and has correct type/value.
        assert_eq!(parsed["schema"], "ftui-evidence-v2");
        assert_eq!(parsed["id"], 42);
        assert_eq!(parsed["ts_ns"], 999_000);
        assert_eq!(parsed["domain"], "diff_strategy");
        assert!(parsed["log_posterior"].as_f64().is_some());
        assert_eq!(parsed["action"], "dirty_rows");
        assert!(parsed["loss_avoided"].as_f64().unwrap() > 0.0);

        // Evidence array.
        let evidence = parsed["evidence"].as_array().expect("evidence is array");
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0]["label"], "change_rate");
        assert!(evidence[0]["bf"].as_f64().unwrap() > 0.0);

        // Confidence interval.
        let ci = parsed["ci"].as_array().expect("ci is array");
        assert_eq!(ci.len(), 2);
        let lower = ci[0].as_f64().unwrap();
        let upper = ci[1].as_f64().unwrap();
        assert!(lower < upper);
    }

    #[test]
    fn jsonl_schema_required_fields_present() {
        // Verify schema compliance for all 7 domains.
        let required_keys = [
            "schema",
            "id",
            "ts_ns",
            "domain",
            "log_posterior",
            "evidence",
            "action",
            "loss_avoided",
            "ci",
        ];

        for (i, domain) in DecisionDomain::ALL.iter().enumerate() {
            let entry = EvidenceEntryBuilder::new(*domain, i as u64, (i as u64 + 1) * 1000)
                .log_posterior(0.5)
                .evidence("test_signal", 2.0)
                .action("test_action")
                .loss_avoided(0.01)
                .confidence_interval(0.4, 0.6)
                .build();

            let jsonl = entry.to_jsonl();
            let parsed: serde_json::Value = serde_json::from_str(&jsonl).unwrap();

            for key in &required_keys {
                assert!(
                    !parsed[key].is_null(),
                    "domain {:?} missing required key '{}'",
                    domain,
                    key
                );
            }

            // Domain string matches enum.
            assert_eq!(parsed["domain"], domain.as_str());
        }
    }

    #[test]
    fn jsonl_backward_compat_extra_fields_ignored() {
        // Simulate a future schema version with extra optional fields.
        // An "old reader" using serde_json::Value should still parse fine.
        let future_jsonl = concat!(
            r#"{"schema":"ftui-evidence-v2","id":1,"ts_ns":5000,"domain":"diff_strategy","#,
            r#""log_posterior":1.386,"evidence":[{"label":"change_rate","bf":4.0}],"#,
            r#""action":"dirty_rows","loss_avoided":0.15,"ci":[0.72,0.95],"#,
            r#""new_optional_field":"future_value","extra_metric":42.5}"#
        );

        let parsed: serde_json::Value =
            serde_json::from_str(future_jsonl).expect("extra fields should not break parsing");

        // Old reader can still access all standard fields.
        assert_eq!(parsed["schema"], "ftui-evidence-v2");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["domain"], "diff_strategy");
        assert_eq!(parsed["action"], "dirty_rows");
        assert!(parsed["log_posterior"].as_f64().is_some());
        assert!(parsed["evidence"].as_array().is_some());
        assert!(parsed["ci"].as_array().is_some());
    }

    #[test]
    fn jsonl_backward_compat_missing_optional_evidence() {
        // An entry with zero evidence terms should still be valid.
        let entry = EvidenceEntryBuilder::new(DecisionDomain::FrameBudget, 0, 1000)
            .log_posterior(0.0)
            .action("hold")
            .build();

        let jsonl = entry.to_jsonl();
        let parsed: serde_json::Value = serde_json::from_str(&jsonl).unwrap();
        let evidence = parsed["evidence"].as_array().unwrap();
        assert!(evidence.is_empty(), "no evidence terms → empty array");
    }

    #[test]
    fn diff_strategy_evidence_format() {
        // Verify that diff strategy evidence from the bridge produces
        // the expected JSONL format.
        let evidence = ftui_render::diff_strategy::StrategyEvidence {
            strategy: ftui_render::diff_strategy::DiffStrategy::DirtyRows,
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
        };

        let entry = crate::evidence_bridges::from_diff_strategy(&evidence, 100_000);
        let jsonl = entry.to_jsonl();
        let parsed: serde_json::Value = serde_json::from_str(&jsonl).unwrap();

        assert_eq!(parsed["domain"], "diff_strategy");
        assert_eq!(parsed["action"], "dirty_rows");

        // Evidence should contain at least change_rate and dirty_ratio.
        let ev_array = parsed["evidence"].as_array().unwrap();
        let labels: Vec<&str> = ev_array
            .iter()
            .map(|e| e["label"].as_str().unwrap())
            .collect();
        assert!(
            labels.contains(&"change_rate"),
            "missing change_rate evidence"
        );
        assert!(
            labels.contains(&"dirty_ratio"),
            "missing dirty_ratio evidence"
        );

        // Confidence interval should be within [0, 1].
        let ci = parsed["ci"].as_array().unwrap();
        let lower = ci[0].as_f64().unwrap();
        let upper = ci[1].as_f64().unwrap();
        assert!(
            (0.0..=1.0).contains(&lower),
            "CI lower out of range: {lower}"
        );
        assert!(
            (0.0..=1.0).contains(&upper),
            "CI upper out of range: {upper}"
        );
        assert!(lower <= upper, "CI lower > upper");
    }
}
