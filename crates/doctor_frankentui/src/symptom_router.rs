// SPDX-License-Identifier: Apache-2.0
//! Symptom router and Fast-Start S-tier candidate harvester.
//!
//! Operationalizes the alien-graveyard intake/route/harvest steps: a symptom
//! (tail latency, correctness, privacy/security, concurrency, adaptive control)
//! is routed through a deterministic path to a candidate pool harvested from the
//! canonical graveyard corpus, then ranked with a Fast-Start bias toward
//! canonical top-ROI (S-tier) entries before speculative picks.
//!
//! # Outputs
//!
//! - A [`SymptomRouter`] that maps a [`SymptomClass`] to a route path and a
//!   candidate pool, then selects an entry under the [`FastStartPolicy`].
//! - [`RoutedRecommendation`] records that reference the routed symptom path,
//!   the candidate pool hash, the canonical entry ids with source provenance,
//!   the mapped migration hotspots, and per-candidate rejection reasons.
//!
//! # Fast-Start bias
//!
//! Candidates are ranked so that S-tier entries clearing the EV threshold are
//! preferred over everything else. When no S-tier candidate clears the
//! threshold, the router either falls back to the best speculative candidate
//! (recording `fallback_used`) or declines to select, depending on policy. An
//! evidence-backed [`PolicyException`] may promote a specific entry ahead of the
//! Fast-Start group.
//!
//! # Determinism
//!
//! Pools are filtered in corpus order, ranked by `(EV desc, entry_id)`, and the
//! pool hash is computed over sorted entry ids, so routing is reproducible.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::milestone_policy::PriorityTier;

// ── Schema Version ───────────────────────────────────────────────────────

/// Schema version for symptom-route reports.
pub const SYMPTOM_ROUTER_SCHEMA_VERSION: &str = "symptom-router-v1";

// ── Symptom Classes ──────────────────────────────────────────────────────

/// A migration symptom that routes to a candidate pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymptomClass {
    /// Tail-latency / p99 regressions.
    TailLatency,
    /// Correctness / semantic divergence.
    Correctness,
    /// Privacy / security exposure.
    PrivacySecurity,
    /// Concurrency bugs (races, deadlocks).
    ConcurrencyBug,
    /// Adaptive-control / feedback instability.
    AdaptiveControl,
}

impl SymptomClass {
    /// Every symptom class, in canonical order.
    pub const ALL: &'static [SymptomClass] = &[
        SymptomClass::TailLatency,
        SymptomClass::Correctness,
        SymptomClass::PrivacySecurity,
        SymptomClass::ConcurrencyBug,
        SymptomClass::AdaptiveControl,
    ];

    /// Stable lowercase tag (also the `symptom_id`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TailLatency => "tail_latency",
            Self::Correctness => "correctness",
            Self::PrivacySecurity => "privacy_security",
            Self::ConcurrencyBug => "concurrency_bug",
            Self::AdaptiveControl => "adaptive_control",
        }
    }

    /// The corpus partition this symptom routes into.
    #[must_use]
    pub fn corpus_partition(self) -> &'static str {
        match self {
            Self::TailLatency => "perf-corpus",
            Self::Correctness => "formal-corpus",
            Self::PrivacySecurity => "security-corpus",
            Self::ConcurrencyBug => "concurrency-corpus",
            Self::AdaptiveControl => "control-corpus",
        }
    }

    /// Deterministic route path: intake -> route -> harvest.
    #[must_use]
    pub fn route_path(self) -> Vec<String> {
        vec![
            format!("intake:{}", self.as_str()),
            format!("route:{}", self.corpus_partition()),
            format!("harvest:s-tier-{}", self.as_str()),
        ]
    }
}

// ── Corpus & Hotspots ────────────────────────────────────────────────────

/// A canonical graveyard-corpus entry (a harvested candidate).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalEntry {
    /// Stable canonical entry id.
    pub entry_id: String,
    /// Human-readable title.
    pub title: String,
    /// Priority tier.
    pub tier: PriorityTier,
    /// Expected-value score.
    pub ev_score: f64,
    /// Symptoms this entry addresses.
    pub symptom_tags: Vec<SymptomClass>,
    /// Archetype label.
    pub archetype: String,
    /// Source corpus identifier (provenance).
    pub source_corpus: String,
    /// Source reference (section / document, provenance).
    pub source_ref: String,
}

impl CanonicalEntry {
    /// Build an entry.
    #[must_use]
    pub fn new(
        entry_id: impl Into<String>,
        title: impl Into<String>,
        tier: PriorityTier,
        ev_score: f64,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            title: title.into(),
            tier,
            ev_score: if ev_score.is_finite() { ev_score } else { 0.0 },
            symptom_tags: Vec::new(),
            archetype: String::new(),
            source_corpus: String::new(),
            source_ref: String::new(),
        }
    }

    /// Attach symptom tags.
    #[must_use]
    pub fn with_symptoms(mut self, symptoms: impl IntoIterator<Item = SymptomClass>) -> Self {
        for symptom in symptoms {
            if !self.symptom_tags.contains(&symptom) {
                self.symptom_tags.push(symptom);
            }
        }
        self
    }

    /// Set the archetype.
    #[must_use]
    pub fn with_archetype(mut self, archetype: impl Into<String>) -> Self {
        self.archetype = archetype.into();
        self
    }

    /// Set the provenance (corpus + reference).
    #[must_use]
    pub fn with_provenance(
        mut self,
        source_corpus: impl Into<String>,
        source_ref: impl Into<String>,
    ) -> Self {
        self.source_corpus = source_corpus.into();
        self.source_ref = source_ref.into();
        self
    }

    /// Whether this entry addresses the given symptom.
    #[must_use]
    pub fn addresses(&self, symptom: SymptomClass) -> bool {
        self.symptom_tags.contains(&symptom)
    }
}

/// A migration hotspot a candidate can be mapped onto.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MigrationHotspot {
    /// Stable hotspot id.
    pub hotspot_id: String,
    /// Symptom this hotspot exhibits.
    pub symptom: SymptomClass,
    /// Code location / subsystem.
    pub location: String,
    /// Severity score (higher = worse).
    pub severity_score: f64,
}

impl MigrationHotspot {
    /// Build a hotspot.
    #[must_use]
    pub fn new(
        hotspot_id: impl Into<String>,
        symptom: SymptomClass,
        location: impl Into<String>,
        severity_score: f64,
    ) -> Self {
        Self {
            hotspot_id: hotspot_id.into(),
            symptom,
            location: location.into(),
            severity_score: if severity_score.is_finite() {
                severity_score
            } else {
                0.0
            },
        }
    }
}

// ── Policy ───────────────────────────────────────────────────────────────

/// Fast-Start ranking policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FastStartPolicy {
    /// EV threshold an S-tier candidate must clear to be Fast-Start eligible.
    pub s_tier_ev_threshold: f64,
    /// Whether to fall back to a speculative candidate when no S-tier clears.
    pub allow_speculative_fallback: bool,
}

impl Default for FastStartPolicy {
    fn default() -> Self {
        Self {
            s_tier_ev_threshold: 8.0,
            allow_speculative_fallback: true,
        }
    }
}

impl FastStartPolicy {
    /// Set the S-tier EV threshold.
    #[must_use]
    pub fn with_s_tier_ev_threshold(mut self, threshold: f64) -> Self {
        self.s_tier_ev_threshold = threshold;
        self
    }

    /// Toggle speculative fallback.
    #[must_use]
    pub fn with_speculative_fallback(mut self, allow: bool) -> Self {
        self.allow_speculative_fallback = allow;
        self
    }
}

/// An evidence-backed exception that promotes a specific entry ahead of the
/// Fast-Start group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyException {
    /// Entry id to promote.
    pub entry_id: String,
    /// Rationale for the exception.
    pub reason: String,
    /// Evidence references backing the exception (must be non-empty).
    pub evidence_refs: Vec<String>,
}

impl PolicyException {
    /// Build a policy exception.
    #[must_use]
    pub fn new(
        entry_id: impl Into<String>,
        reason: impl Into<String>,
        evidence_refs: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            reason: reason.into(),
            evidence_refs: evidence_refs.into_iter().collect(),
        }
    }

    /// Whether this exception is evidence-backed (has at least one reference).
    #[must_use]
    pub fn is_evidence_backed(&self) -> bool {
        !self.reason.trim().is_empty() && self.evidence_refs.iter().any(|r| !r.trim().is_empty())
    }
}

// ── Recommendation ───────────────────────────────────────────────────────

/// Why a candidate was not selected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectedCandidate {
    /// Entry id.
    pub entry_id: String,
    /// Tier.
    pub tier: PriorityTier,
    /// EV score.
    pub ev_score: f64,
    /// Rejection reason.
    pub reason: String,
}

/// The routed recommendation for one symptom.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutedRecommendation {
    /// Symptom id (`symptom.as_str()`).
    pub symptom_id: String,
    /// The symptom class.
    pub symptom: SymptomClass,
    /// Deterministic route path.
    pub route_path: Vec<String>,
    /// Hash of the candidate pool (over sorted entry ids).
    pub candidate_pool_hash: String,
    /// Candidate pool entry ids (in corpus order).
    pub candidate_pool_ids: Vec<String>,
    /// Selected entry id (none if the pool was empty or selection declined).
    pub selected_entry_id: Option<String>,
    /// Selected entry tier.
    pub selected_tier: Option<PriorityTier>,
    /// Canonical entry ids referenced (the pool), for provenance.
    pub canonical_entry_ids: Vec<String>,
    /// Source provenance (`source_corpus:source_ref`) for each pool entry.
    pub source_provenance: Vec<String>,
    /// Mapped migration hotspot ids.
    pub mapped_hotspots: Vec<String>,
    /// Whether the Fast-Start S-tier bias produced the selection.
    pub fast_start_applied: bool,
    /// Whether a speculative fallback was used.
    pub fallback_used: bool,
    /// Whether an evidence-backed policy exception drove the selection.
    pub policy_exception_applied: bool,
    /// Rejected candidates with reasons.
    pub rejected: Vec<RejectedCandidate>,
}

// ── Router ───────────────────────────────────────────────────────────────

/// Routes symptoms to harvested candidates under the Fast-Start policy.
#[derive(Debug, Clone, Default)]
pub struct SymptomRouter {
    policy: FastStartPolicy,
}

impl SymptomRouter {
    /// Build a router from policy.
    #[must_use]
    pub fn new(policy: FastStartPolicy) -> Self {
        Self { policy }
    }

    /// Borrow the policy.
    #[must_use]
    pub fn policy(&self) -> &FastStartPolicy {
        &self.policy
    }

    /// Route a single symptom to a recommendation.
    #[must_use]
    pub fn route(
        &self,
        symptom: SymptomClass,
        corpus: &[CanonicalEntry],
        hotspots: &[MigrationHotspot],
        exceptions: &[PolicyException],
    ) -> RoutedRecommendation {
        // Harvest: filter the corpus to entries addressing this symptom, in order.
        let pool: Vec<&CanonicalEntry> = corpus.iter().filter(|e| e.addresses(symptom)).collect();
        let candidate_pool_ids: Vec<String> = pool.iter().map(|e| e.entry_id.clone()).collect();
        let candidate_pool_hash = pool_hash(&candidate_pool_ids);
        let canonical_entry_ids = candidate_pool_ids.clone();
        let source_provenance: Vec<String> = pool
            .iter()
            .map(|e| format!("{}:{}", e.source_corpus, e.source_ref))
            .collect();
        let mapped_hotspots: Vec<String> = hotspots
            .iter()
            .filter(|h| h.symptom == symptom)
            .map(|h| h.hotspot_id.clone())
            .collect();

        let ranked = self.rank(&pool, exceptions);
        let selection = self.select(&ranked, exceptions);

        let rejected: Vec<RejectedCandidate> = ranked
            .iter()
            .filter(|c| Some(&c.entry.entry_id) != selection.selected.as_ref())
            .map(|c| RejectedCandidate {
                entry_id: c.entry.entry_id.clone(),
                tier: c.entry.tier,
                ev_score: c.entry.ev_score,
                reason: c.rejection_reason(&self.policy),
            })
            .collect();

        let selected_tier = selection
            .selected
            .as_ref()
            .and_then(|id| pool.iter().find(|e| &e.entry_id == id).map(|e| e.tier));

        RoutedRecommendation {
            symptom_id: symptom.as_str().to_string(),
            symptom,
            route_path: symptom.route_path(),
            candidate_pool_hash,
            candidate_pool_ids,
            selected_entry_id: selection.selected,
            selected_tier,
            canonical_entry_ids,
            source_provenance,
            mapped_hotspots,
            fast_start_applied: selection.fast_start_applied,
            fallback_used: selection.fallback_used,
            policy_exception_applied: selection.policy_exception_applied,
            rejected,
        }
    }

    /// Route every symptom class in canonical order into a report.
    #[must_use]
    pub fn evaluate(
        &self,
        corpus: &[CanonicalEntry],
        hotspots: &[MigrationHotspot],
        exceptions: &[PolicyException],
    ) -> SymptomRouteReport {
        let recommendations: Vec<RoutedRecommendation> = SymptomClass::ALL
            .iter()
            .copied()
            .map(|symptom| self.route(symptom, corpus, hotspots, exceptions))
            .collect();

        let summary = summarize(&recommendations);
        let report_id = report_id_for(&self.policy, corpus, exceptions);
        let exported_json_stats =
            export_json_stats(&report_id, &self.policy, &recommendations, &summary);
        let replay_command = format!(
            "doctor_frankentui symptom-route --report-id {report_id} \
             --s-tier-ev {:.3} --speculative-fallback {}",
            self.policy.s_tier_ev_threshold, self.policy.allow_speculative_fallback
        );

        SymptomRouteReport {
            schema_version: SYMPTOM_ROUTER_SCHEMA_VERSION.to_string(),
            report_id,
            policy: self.policy.clone(),
            recommendations,
            summary,
            exported_json_stats,
            replay_command,
        }
    }

    /// Rank the pool: exception-promoted first, then Fast-Start S-tier, then
    /// the rest, each sorted by `(EV desc, entry_id)`.
    fn rank<'a>(
        &self,
        pool: &[&'a CanonicalEntry],
        exceptions: &[PolicyException],
    ) -> Vec<RankedCandidate<'a>> {
        let mut promoted = Vec::new();
        let mut fast = Vec::new();
        let mut others = Vec::new();
        for entry in pool {
            let exception = exceptions
                .iter()
                .find(|e| e.entry_id == entry.entry_id && e.is_evidence_backed());
            if let Some(exc) = exception {
                promoted.push(RankedCandidate {
                    entry,
                    group: RankGroup::Promoted,
                    exception_reason: Some(exc.reason.clone()),
                });
            } else if entry.tier == PriorityTier::S
                && entry.ev_score >= self.policy.s_tier_ev_threshold
            {
                fast.push(RankedCandidate {
                    entry,
                    group: RankGroup::FastStart,
                    exception_reason: None,
                });
            } else {
                others.push(RankedCandidate {
                    entry,
                    group: RankGroup::Speculative,
                    exception_reason: None,
                });
            }
        }
        sort_by_ev(&mut promoted);
        sort_by_ev(&mut fast);
        sort_by_ev(&mut others);
        let mut ranked = promoted;
        ranked.extend(fast);
        ranked.extend(others);
        ranked
    }

    fn select(&self, ranked: &[RankedCandidate], exceptions: &[PolicyException]) -> Selection {
        let Some(first) = ranked.first() else {
            return Selection::none();
        };
        match first.group {
            RankGroup::Promoted => Selection {
                selected: Some(first.entry.entry_id.clone()),
                fast_start_applied: false,
                fallback_used: false,
                policy_exception_applied: exceptions
                    .iter()
                    .any(|e| e.entry_id == first.entry.entry_id && e.is_evidence_backed()),
            },
            RankGroup::FastStart => Selection {
                selected: Some(first.entry.entry_id.clone()),
                fast_start_applied: true,
                fallback_used: false,
                policy_exception_applied: false,
            },
            RankGroup::Speculative => {
                // No S-tier cleared the threshold. Fall back only if allowed.
                let selected = self
                    .policy
                    .allow_speculative_fallback
                    .then(|| first.entry.entry_id.clone());
                Selection {
                    selected,
                    fast_start_applied: false,
                    fallback_used: true,
                    policy_exception_applied: false,
                }
            }
        }
    }
}

// ── Ranking internals ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RankGroup {
    Promoted,
    FastStart,
    Speculative,
}

struct RankedCandidate<'a> {
    entry: &'a CanonicalEntry,
    group: RankGroup,
    exception_reason: Option<String>,
}

impl RankedCandidate<'_> {
    fn rejection_reason(&self, policy: &FastStartPolicy) -> String {
        match self.group {
            RankGroup::Promoted => self.exception_reason.clone().map_or_else(
                || "promoted but not selected".to_string(),
                |reason| format!("policy-exception candidate not selected: {reason}"),
            ),
            RankGroup::FastStart => {
                "S-tier Fast-Start candidate with lower EV than the selected entry".to_string()
            }
            RankGroup::Speculative => {
                if self.entry.tier == PriorityTier::S {
                    format!(
                        "S-tier entry below the Fast-Start EV threshold {:.3}",
                        policy.s_tier_ev_threshold
                    )
                } else {
                    format!(
                        "non-S-tier ({}) speculative candidate deprioritized by Fast-Start bias",
                        self.entry.tier.as_str()
                    )
                }
            }
        }
    }
}

fn sort_by_ev(candidates: &mut [RankedCandidate]) {
    candidates.sort_by(|a, b| {
        b.entry
            .ev_score
            .total_cmp(&a.entry.ev_score)
            .then_with(|| a.entry.entry_id.cmp(&b.entry.entry_id))
    });
}

struct Selection {
    selected: Option<String>,
    fast_start_applied: bool,
    fallback_used: bool,
    policy_exception_applied: bool,
}

impl Selection {
    fn none() -> Self {
        Self {
            selected: None,
            fast_start_applied: false,
            fallback_used: false,
            policy_exception_applied: false,
        }
    }
}

// ── Report ───────────────────────────────────────────────────────────────

/// Aggregate counts for a symptom-route report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymptomRouteSummary {
    /// Symptoms routed.
    pub total_symptoms: usize,
    /// Recommendations with a selected entry.
    pub selected_count: usize,
    /// Selections driven by the Fast-Start S-tier bias.
    pub fast_start_count: usize,
    /// Selections that used a speculative fallback.
    pub fallback_count: usize,
    /// Selections driven by a policy exception.
    pub policy_exception_count: usize,
    /// Symptoms with an empty candidate pool.
    pub empty_pool_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymptomRouteJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full symptom-route report over every symptom class.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SymptomRouteReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Policy used.
    pub policy: FastStartPolicy,
    /// Per-symptom recommendations (canonical symptom order).
    pub recommendations: Vec<RoutedRecommendation>,
    /// Aggregate summary.
    pub summary: SymptomRouteSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: SymptomRouteJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl SymptomRouteReport {
    /// The recommendation for a given symptom, if present.
    #[must_use]
    pub fn recommendation_for(&self, symptom: SymptomClass) -> Option<&RoutedRecommendation> {
        self.recommendations.iter().find(|r| r.symptom == symptom)
    }
}

// ── Canonical Corpus ─────────────────────────────────────────────────────

/// A built-in canonical graveyard corpus biased toward S-tier top-ROI entries,
/// covering every symptom class. Useful as a default harvest source.
#[must_use]
pub fn canonical_corpus() -> Vec<CanonicalEntry> {
    vec![
        CanonicalEntry::new(
            "gv-erasure-coding",
            "Reed-Solomon tail-latency hedging",
            PriorityTier::S,
            9.2,
        )
        .with_symptoms([SymptomClass::TailLatency])
        .with_archetype("tail-latency-hedge")
        .with_provenance("perf-corpus", "graveyard §6.1"),
        CanonicalEntry::new(
            "gv-adaptive-batch",
            "Adaptive batch sizing for p99",
            PriorityTier::A,
            6.0,
        )
        .with_symptoms([SymptomClass::TailLatency])
        .with_archetype("adaptive-batch")
        .with_provenance("perf-corpus", "graveyard §6.4"),
        CanonicalEntry::new(
            "gv-metamorphic-equiv",
            "Metamorphic equivalence oracle",
            PriorityTier::S,
            8.8,
        )
        .with_symptoms([SymptomClass::Correctness])
        .with_archetype("equivalence-oracle")
        .with_provenance("formal-corpus", "graveyard §3.2"),
        CanonicalEntry::new(
            "gv-abstract-interp",
            "Abstract-interpretation safety net",
            PriorityTier::A,
            6.5,
        )
        .with_symptoms([SymptomClass::Correctness])
        .with_archetype("over-approximation")
        .with_provenance("formal-corpus", "graveyard §3.5"),
        CanonicalEntry::new(
            "gv-dp-redaction",
            "Differential-privacy redaction budget",
            PriorityTier::S,
            8.5,
        )
        .with_symptoms([SymptomClass::PrivacySecurity])
        .with_archetype("privacy-budget")
        .with_provenance("security-corpus", "graveyard §8.3"),
        CanonicalEntry::new(
            "gv-lockfree-queue",
            "Lock-free MPSC with hazard pointers",
            PriorityTier::S,
            8.9,
        )
        .with_symptoms([SymptomClass::ConcurrencyBug])
        .with_archetype("lock-free-structure")
        .with_provenance("concurrency-corpus", "graveyard §2.7"),
        CanonicalEntry::new(
            "gv-deadlock-order",
            "Lock-ordering deadlock prevention",
            PriorityTier::A,
            5.5,
        )
        .with_symptoms([SymptomClass::ConcurrencyBug])
        .with_archetype("lock-ordering")
        .with_provenance("concurrency-corpus", "graveyard §2.9"),
        CanonicalEntry::new(
            "gv-mpc-control",
            "Model-predictive autoscaler control",
            PriorityTier::S,
            8.6,
        )
        .with_symptoms([SymptomClass::AdaptiveControl])
        .with_archetype("mpc-controller")
        .with_provenance("control-corpus", "graveyard §5.4"),
        CanonicalEntry::new(
            "gv-pid-tuning",
            "Auto-tuned PID feedback",
            PriorityTier::B,
            3.0,
        )
        .with_symptoms([SymptomClass::AdaptiveControl])
        .with_archetype("pid-controller")
        .with_provenance("control-corpus", "graveyard §5.1"),
    ]
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn summarize(recommendations: &[RoutedRecommendation]) -> SymptomRouteSummary {
    let mut summary = SymptomRouteSummary {
        total_symptoms: recommendations.len(),
        selected_count: 0,
        fast_start_count: 0,
        fallback_count: 0,
        policy_exception_count: 0,
        empty_pool_count: 0,
    };
    for rec in recommendations {
        if rec.selected_entry_id.is_some() {
            summary.selected_count += 1;
        }
        if rec.fast_start_applied {
            summary.fast_start_count += 1;
        }
        if rec.fallback_used {
            summary.fallback_count += 1;
        }
        if rec.policy_exception_applied {
            summary.policy_exception_count += 1;
        }
        if rec.candidate_pool_ids.is_empty() {
            summary.empty_pool_count += 1;
        }
    }
    summary
}

fn pool_hash(entry_ids: &[String]) -> String {
    let mut sorted: Vec<&String> = entry_ids.iter().collect();
    sorted.sort();
    let joined = sorted
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    format!("pool-{}", short_hash(&sha256_hex(joined.as_bytes())))
}

fn export_json_stats(
    report_id: &str,
    policy: &FastStartPolicy,
    recommendations: &[RoutedRecommendation],
    summary: &SymptomRouteSummary,
) -> SymptomRouteJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        policy: &'a FastStartPolicy,
        summary: &'a SymptomRouteSummary,
        recommendations: &'a [RoutedRecommendation],
    }

    let payload = Export {
        schema_version: SYMPTOM_ROUTER_SCHEMA_VERSION,
        report_id,
        policy,
        summary,
        recommendations,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    SymptomRouteJsonStatsArtifact {
        path: format!("{report_id}/symptom_route_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(
    policy: &FastStartPolicy,
    corpus: &[CanonicalEntry],
    exceptions: &[PolicyException],
) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        policy: &'a FastStartPolicy,
        corpus: &'a [CanonicalEntry],
        exceptions: &'a [PolicyException],
    }
    let hash = stable_hash(&IdInput {
        policy,
        corpus,
        exceptions,
    });
    format!("symptom-router-{}", short_hash(&hash))
}

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> SymptomRouter {
        SymptomRouter::default()
    }

    #[test]
    fn route_path_is_deterministic_and_three_step() {
        for symptom in SymptomClass::ALL.iter().copied() {
            let path = symptom.route_path();
            assert_eq!(path.len(), 3);
            assert!(path[0].starts_with("intake:"));
            assert!(path[1].starts_with("route:"));
            assert!(path[2].starts_with("harvest:s-tier-"));
            assert_eq!(symptom.route_path(), path);
        }
    }

    #[test]
    fn canonical_corpus_covers_every_symptom() {
        let corpus = canonical_corpus();
        for symptom in SymptomClass::ALL.iter().copied() {
            assert!(
                corpus.iter().any(|e| e.addresses(symptom)),
                "no corpus entry for {symptom:?}"
            );
        }
    }

    #[test]
    fn fast_start_prefers_s_tier_over_higher_pool_position() {
        // Tail latency: gv-erasure-coding (S, 9.2) should beat gv-adaptive-batch (A, 6.0).
        let rec = router().route(SymptomClass::TailLatency, &canonical_corpus(), &[], &[]);
        assert_eq!(rec.selected_entry_id.as_deref(), Some("gv-erasure-coding"));
        assert_eq!(rec.selected_tier, Some(PriorityTier::S));
        assert!(rec.fast_start_applied);
        assert!(!rec.fallback_used);
        assert!(!rec.policy_exception_applied);
        // The A-tier candidate is rejected with a Fast-Start reason.
        assert!(
            rec.rejected
                .iter()
                .any(|r| r.entry_id == "gv-adaptive-batch")
        );
    }

    #[test]
    fn recommendation_references_provenance_and_pool() {
        let rec = router().route(SymptomClass::Correctness, &canonical_corpus(), &[], &[]);
        assert!(!rec.canonical_entry_ids.is_empty());
        assert!(rec.source_provenance.iter().all(|p| p.contains("corpus")));
        assert!(rec.candidate_pool_hash.starts_with("pool-"));
        assert_eq!(rec.symptom_id, "correctness");
    }

    #[test]
    fn empty_pool_yields_no_selection() {
        // A corpus with no entries for the symptom.
        let corpus = vec![
            CanonicalEntry::new("x", "x", PriorityTier::S, 9.0)
                .with_symptoms([SymptomClass::TailLatency]),
        ];
        let rec = router().route(SymptomClass::PrivacySecurity, &corpus, &[], &[]);
        assert!(rec.selected_entry_id.is_none());
        assert!(rec.candidate_pool_ids.is_empty());
        assert!(rec.rejected.is_empty());
    }

    #[test]
    fn fallback_used_when_no_s_tier_clears_threshold() {
        // Only a B-tier candidate for adaptive control if we raise the threshold.
        let corpus = vec![
            CanonicalEntry::new("only-b", "b", PriorityTier::B, 3.0)
                .with_symptoms([SymptomClass::AdaptiveControl]),
        ];
        let rec = router().route(SymptomClass::AdaptiveControl, &corpus, &[], &[]);
        assert_eq!(rec.selected_entry_id.as_deref(), Some("only-b"));
        assert!(rec.fallback_used);
        assert!(!rec.fast_start_applied);
    }

    #[test]
    fn fallback_can_be_disallowed() {
        let corpus = vec![
            CanonicalEntry::new("only-b", "b", PriorityTier::B, 3.0)
                .with_symptoms([SymptomClass::AdaptiveControl]),
        ];
        let policy = FastStartPolicy::default().with_speculative_fallback(false);
        let rec =
            SymptomRouter::new(policy).route(SymptomClass::AdaptiveControl, &corpus, &[], &[]);
        assert!(rec.selected_entry_id.is_none());
        assert!(rec.fallback_used);
    }

    #[test]
    fn s_tier_below_threshold_is_not_fast_start() {
        let corpus = vec![
            CanonicalEntry::new("weak-s", "s", PriorityTier::S, 4.0)
                .with_symptoms([SymptomClass::Correctness]),
        ];
        let rec = router().route(SymptomClass::Correctness, &corpus, &[], &[]);
        // S-tier but below the 8.0 threshold → speculative fallback, not Fast-Start.
        assert_eq!(rec.selected_entry_id.as_deref(), Some("weak-s"));
        assert!(!rec.fast_start_applied);
        assert!(rec.fallback_used);
    }

    #[test]
    fn evidence_backed_exception_promotes_a_candidate() {
        let exceptions = vec![PolicyException::new(
            "gv-adaptive-batch",
            "validated lower tail risk in this codebase",
            ["bench-run-42".to_string()],
        )];
        let rec = router().route(
            SymptomClass::TailLatency,
            &canonical_corpus(),
            &[],
            &exceptions,
        );
        // The exception promotes the A-tier entry ahead of the S-tier Fast-Start pick.
        assert_eq!(rec.selected_entry_id.as_deref(), Some("gv-adaptive-batch"));
        assert!(rec.policy_exception_applied);
        assert!(!rec.fast_start_applied);
    }

    #[test]
    fn exception_without_evidence_is_ignored() {
        let exceptions = vec![PolicyException::new(
            "gv-adaptive-batch",
            "hunch",
            Vec::new(),
        )];
        let rec = router().route(
            SymptomClass::TailLatency,
            &canonical_corpus(),
            &[],
            &exceptions,
        );
        // No evidence → exception ignored, Fast-Start S-tier wins.
        assert_eq!(rec.selected_entry_id.as_deref(), Some("gv-erasure-coding"));
        assert!(!rec.policy_exception_applied);
        assert!(rec.fast_start_applied);
    }

    #[test]
    fn hotspots_are_mapped_by_symptom() {
        let hotspots = vec![
            MigrationHotspot::new("hs-1", SymptomClass::TailLatency, "render::flush", 0.8),
            MigrationHotspot::new("hs-2", SymptomClass::Correctness, "diff::compute", 0.5),
        ];
        let rec = router().route(
            SymptomClass::TailLatency,
            &canonical_corpus(),
            &hotspots,
            &[],
        );
        assert_eq!(rec.mapped_hotspots, vec!["hs-1".to_string()]);
    }

    #[test]
    fn evaluate_routes_every_symptom() {
        let report = router().evaluate(&canonical_corpus(), &[], &[]);
        assert_eq!(report.recommendations.len(), SymptomClass::ALL.len());
        assert_eq!(report.summary.total_symptoms, 5);
        // Every symptom in the canonical corpus has an S-tier Fast-Start pick.
        assert_eq!(report.summary.fast_start_count, 5);
        assert_eq!(report.summary.selected_count, 5);
        for symptom in SymptomClass::ALL.iter().copied() {
            assert!(report.recommendation_for(symptom).is_some());
        }
    }

    #[test]
    fn report_is_deterministic() {
        let corpus = canonical_corpus();
        let first = router().evaluate(&corpus, &[], &[]);
        let second = router().evaluate(&corpus, &[], &[]);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.recommendations, second.recommendations);
    }

    #[test]
    fn pool_hash_is_order_independent() {
        let a = pool_hash(&["a".to_string(), "b".to_string()]);
        let b = pool_hash(&["b".to_string(), "a".to_string()]);
        assert_eq!(a, b);
        let c = pool_hash(&["a".to_string(), "c".to_string()]);
        assert_ne!(a, c);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = router().evaluate(&canonical_corpus(), &[], &[]);
        let json = serde_json::to_string(&report).unwrap();
        let restored: SymptomRouteReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.recommendations, report.recommendations);
        assert_eq!(restored.summary, report.summary);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let report = router().evaluate(&canonical_corpus(), &[], &[]);
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn replay_command_references_report() {
        let report = router().evaluate(&canonical_corpus(), &[], &[]);
        assert!(report.replay_command.contains("symptom-route"));
        assert!(report.replay_command.contains(&report.report_id));
    }

    #[test]
    fn rejections_carry_reasons() {
        let rec = router().route(SymptomClass::TailLatency, &canonical_corpus(), &[], &[]);
        assert!(!rec.rejected.is_empty());
        assert!(rec.rejected.iter().all(|r| !r.reason.trim().is_empty()));
    }

    #[test]
    fn higher_ev_s_tier_wins_tie_break_is_entry_id() {
        // Two S-tier entries with equal EV → entry_id breaks the tie deterministically.
        let corpus = vec![
            CanonicalEntry::new("s-bbb", "b", PriorityTier::S, 9.0)
                .with_symptoms([SymptomClass::Correctness]),
            CanonicalEntry::new("s-aaa", "a", PriorityTier::S, 9.0)
                .with_symptoms([SymptomClass::Correctness]),
        ];
        let rec = router().route(SymptomClass::Correctness, &corpus, &[], &[]);
        assert_eq!(rec.selected_entry_id.as_deref(), Some("s-aaa"));
    }
}
