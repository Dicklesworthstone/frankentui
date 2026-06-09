//! Shadow-run dual execution harness for safe migration adoption.
//!
//! Shadow runs mirror the same canonical traffic into source OpenTUI and
//! translated FrankenTUI executions, capture both traces, and gate promotion on
//! sustained semantic stability rather than a single passing comparison.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::metamorphic_oracle::{
    MetamorphicOracleConfig, MetamorphicOracleReport, compare_traces_with_metamorphic_oracle,
};
use crate::semantic_diff::SemanticDiffVerdict;
use crate::trace::{InteractionTrace, TraceBuilder, TracePayload, validate_trace};

/// Schema version for shadow-run reports and evidence payloads.
pub const SHADOW_RUN_SCHEMA_VERSION: &str = "shadow-run-v1";

/// Explicit thresholds that decide whether a shadow batch must hold back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShadowDivergenceThresholds {
    /// Maximum allowed violating-run rate, measured in per-mille units.
    pub max_violation_rate_per_mille: u32,
    /// Maximum consecutive final semantic violations allowed in one batch.
    pub max_consecutive_violations: usize,
    /// Maximum accepted semantic risk score for a final violation.
    pub max_violation_risk_score: f64,
    /// Require source and translated traces to prove identical mirrored inputs.
    pub require_mirrored_input_match: bool,
}

impl Default for ShadowDivergenceThresholds {
    fn default() -> Self {
        Self {
            max_violation_rate_per_mille: 0,
            max_consecutive_violations: 0,
            max_violation_risk_score: 0.0,
            require_mirrored_input_match: true,
        }
    }
}

/// Policy switches for conservative holdback behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowHoldbackPolicy {
    pub holdback_on_input_mismatch: bool,
    pub holdback_on_semantic_violation: bool,
    pub holdback_on_threshold_breach: bool,
    pub holdback_on_invalid_trace: bool,
}

impl Default for ShadowHoldbackPolicy {
    fn default() -> Self {
        Self {
            holdback_on_input_mismatch: true,
            holdback_on_semantic_violation: true,
            holdback_on_threshold_breach: true,
            holdback_on_invalid_trace: true,
        }
    }
}

/// Minimum evidence required before promotion can leave shadow mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowPromotionPolicy {
    /// Minimum observed shadow pairs in the stability window.
    pub min_total_runs: usize,
    /// Minimum stable pairs in the stability window.
    pub required_stable_runs: usize,
    /// Minimum summed trace duration covered by the stability window.
    pub required_stable_duration_ms: u64,
    /// Require every promoted pair to have matched mirrored-input evidence.
    pub require_all_inputs_mirrored: bool,
    /// Require no active holdback reason in the batch.
    pub require_no_holdbacks: bool,
}

impl Default for ShadowPromotionPolicy {
    fn default() -> Self {
        Self {
            min_total_runs: 3,
            required_stable_runs: 3,
            required_stable_duration_ms: 60_000,
            require_all_inputs_mirrored: true,
            require_no_holdbacks: true,
        }
    }
}

/// Shadow-run harness configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShadowRunConfig {
    pub policy_id: String,
    pub oracle_config: MetamorphicOracleConfig,
    pub divergence_thresholds: ShadowDivergenceThresholds,
    pub holdback_policy: ShadowHoldbackPolicy,
    pub promotion_policy: ShadowPromotionPolicy,
}

impl ShadowRunConfig {
    /// Production default: tolerate schedule drift but require sustained proof
    /// before promotion.
    #[must_use]
    pub fn production_default() -> Self {
        Self {
            policy_id: "shadow-run-production-default-v1".to_string(),
            oracle_config: MetamorphicOracleConfig::schedule_tolerant(),
            divergence_thresholds: ShadowDivergenceThresholds::default(),
            holdback_policy: ShadowHoldbackPolicy::default(),
            promotion_policy: ShadowPromotionPolicy::default(),
        }
    }

    /// Strict semantic comparison for deterministic certification lanes.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            policy_id: "shadow-run-strict-v1".to_string(),
            oracle_config: MetamorphicOracleConfig::strict(),
            divergence_thresholds: ShadowDivergenceThresholds::default(),
            holdback_policy: ShadowHoldbackPolicy::default(),
            promotion_policy: ShadowPromotionPolicy::default(),
        }
    }

    /// Lenient promotion policy for unit fixtures and local smoke gates.
    #[must_use]
    pub fn smoke_test() -> Self {
        let mut config = Self::production_default();
        config.policy_id = "shadow-run-smoke-test-v1".to_string();
        config.promotion_policy = ShadowPromotionPolicy {
            min_total_runs: 1,
            required_stable_runs: 1,
            required_stable_duration_ms: 0,
            require_all_inputs_mirrored: true,
            require_no_holdbacks: true,
        };
        config
    }
}

impl Default for ShadowRunConfig {
    fn default() -> Self {
        Self::production_default()
    }
}

/// Source and translated traces captured for one mirrored traffic sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowTracePair {
    pub mirror_id: String,
    pub source_trace: InteractionTrace,
    pub translated_trace: InteractionTrace,
    pub input_evidence: ShadowInputEvidence,
}

impl ShadowTracePair {
    /// Build a pair from already captured source and translated traces.
    #[must_use]
    pub fn new(
        mirror_id: impl Into<String>,
        source_trace: InteractionTrace,
        translated_trace: InteractionTrace,
    ) -> Self {
        let input_evidence = ShadowInputEvidence::from_traces(&source_trace, &translated_trace);
        Self {
            mirror_id: mirror_id.into(),
            source_trace,
            translated_trace,
            input_evidence,
        }
    }
}

/// Deterministic proof that source and translated runs received the same input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowInputEvidence {
    pub source_input_hash: String,
    pub translated_input_hash: String,
    pub source_input_events: usize,
    pub translated_input_events: usize,
    pub input_hash_matched: bool,
}

impl ShadowInputEvidence {
    #[must_use]
    pub fn from_traces(
        source_trace: &InteractionTrace,
        translated_trace: &InteractionTrace,
    ) -> Self {
        let source_inputs = input_fingerprints(source_trace);
        let translated_inputs = input_fingerprints(translated_trace);
        let source_input_hash = stable_hash(&source_inputs);
        let translated_input_hash = stable_hash(&translated_inputs);
        let input_hash_matched = source_input_hash
            .as_str()
            .cmp(translated_input_hash.as_str())
            .is_eq();
        Self {
            source_input_hash,
            translated_input_hash,
            source_input_events: source_inputs.len(),
            translated_input_events: translated_inputs.len(),
            input_hash_matched,
        }
    }
}

/// One structured threshold breach observed by the harness.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShadowThresholdBreach {
    InputMirrorMismatch {
        source_input_hash: String,
        translated_input_hash: String,
    },
    InvalidTrace {
        side: ShadowTraceSide,
        message: String,
    },
    SemanticViolation {
        violated_relation_ids: Vec<String>,
        violated_clause_ids: Vec<String>,
    },
    ViolationRiskScore {
        risk_score_basis_points: u32,
        max_allowed_basis_points: u32,
    },
    ViolationRate {
        observed_per_mille: u32,
        max_allowed_per_mille: u32,
    },
    ConsecutiveViolations {
        observed: usize,
        max_allowed: usize,
    },
}

/// Side of a shadow pair.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShadowTraceSide {
    Source,
    Translated,
}

/// Per-run shadow comparison observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowRunObservation {
    pub mirror_id: String,
    pub source_trace_id: String,
    pub translated_trace_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub input_evidence: ShadowInputEvidence,
    pub oracle_report: MetamorphicOracleReport,
    pub threshold_breaches: Vec<ShadowThresholdBreach>,
    pub stable: bool,
    pub duration_ms: u64,
}

/// Aggregated stability window that promotion decisions must satisfy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowStabilityEvidence {
    pub total_runs: usize,
    pub stable_runs: usize,
    pub violating_runs: usize,
    pub input_mismatch_runs: usize,
    pub total_duration_ms: u64,
    pub max_consecutive_violations: usize,
    pub violation_rate_per_mille: u32,
    pub stable_mirror_ids: Vec<String>,
    pub violated_mirror_ids: Vec<String>,
}

/// Final decision for a shadow-run batch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShadowPromotionDecision {
    Promote,
    ContinueShadow,
    Holdback,
}

/// Structured reason keeping the translated app in shadow mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowHoldbackReason {
    pub reason_id: String,
    pub affected_mirror_ids: Vec<String>,
    pub message: String,
}

/// Full shadow-run report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowRunReport {
    pub schema_version: String,
    pub report_id: String,
    pub policy_id: String,
    pub oracle_catalog_version: String,
    pub observations: Vec<ShadowRunObservation>,
    pub stability_evidence: ShadowStabilityEvidence,
    pub decision: ShadowPromotionDecision,
    pub holdback_reasons: Vec<ShadowHoldbackReason>,
    pub evidence_checksum: String,
}

/// Deterministic pure harness over captured traces.
#[derive(Debug, Clone)]
pub struct ShadowRunHarness {
    config: ShadowRunConfig,
}

impl ShadowRunHarness {
    /// Create a harness with explicit policy.
    #[must_use]
    pub fn new(config: ShadowRunConfig) -> Self {
        Self { config }
    }

    /// Create the production default harness.
    #[must_use]
    pub fn production_default() -> Self {
        Self::new(ShadowRunConfig::production_default())
    }

    /// Access the configured policy.
    #[must_use]
    pub fn config(&self) -> &ShadowRunConfig {
        &self.config
    }

    /// Compare one source/translated trace pair.
    #[must_use]
    pub fn evaluate_pair(&self, pair: ShadowTracePair) -> ShadowRunObservation {
        let oracle_report = compare_traces_with_metamorphic_oracle(
            &pair.source_trace,
            &pair.translated_trace,
            &self.config.oracle_config,
        );
        let threshold_breaches =
            per_run_breaches(&pair, &oracle_report, &self.config.divergence_thresholds);
        let stable = threshold_breaches.is_empty()
            && !matches!(oracle_report.verdict, SemanticDiffVerdict::Violation);
        let duration_ms = pair
            .source_trace
            .duration_ms
            .max(pair.translated_trace.duration_ms);

        ShadowRunObservation {
            mirror_id: pair.mirror_id,
            source_trace_id: pair.source_trace.trace_id,
            translated_trace_id: pair.translated_trace.trace_id,
            source_run_id: pair.source_trace.run_id,
            translated_run_id: pair.translated_trace.run_id,
            input_evidence: pair.input_evidence,
            oracle_report,
            threshold_breaches,
            stable,
            duration_ms,
        }
    }

    /// Compare a stability window of source/translated trace pairs.
    #[must_use]
    pub fn evaluate_pairs(&self, pairs: Vec<ShadowTracePair>) -> ShadowRunReport {
        let observations = pairs
            .into_iter()
            .map(|pair| self.evaluate_pair(pair))
            .collect::<Vec<_>>();
        let stability_evidence = stability_evidence(&observations);
        let mut holdback_reasons =
            holdback_reasons(&observations, &stability_evidence, &self.config);
        let decision = if !holdback_reasons.is_empty() {
            ShadowPromotionDecision::Holdback
        } else if promotion_ready(&stability_evidence, &self.config.promotion_policy) {
            ShadowPromotionDecision::Promote
        } else {
            ShadowPromotionDecision::ContinueShadow
        };

        if matches!(decision, ShadowPromotionDecision::ContinueShadow) {
            holdback_reasons.clear();
        }

        let evidence_checksum = report_evidence_checksum(&observations, &stability_evidence);
        let report_id = format!("shadow-report-{}", short_hash(&evidence_checksum));

        ShadowRunReport {
            schema_version: SHADOW_RUN_SCHEMA_VERSION.to_string(),
            report_id,
            policy_id: self.config.policy_id.clone(),
            oracle_catalog_version: self.config.oracle_config.catalog_version.clone(),
            observations,
            stability_evidence,
            decision,
            holdback_reasons,
            evidence_checksum,
        }
    }
}

/// Build source and translated input traces from one canonical traffic trace.
///
/// Output captures are deliberately omitted because this function is the
/// traffic-mirroring layer. Runtime capture code should append render/state
/// evidence to each side before passing the pair to [`ShadowRunHarness`].
#[must_use]
pub fn mirror_input_trace(
    canonical_trace: &InteractionTrace,
    mirror_id: impl Into<String>,
    source_run_id: impl Into<String>,
    translated_run_id: impl Into<String>,
) -> ShadowTracePair {
    let mirror_id = mirror_id.into();
    let source_run_id = source_run_id.into();
    let translated_run_id = translated_run_id.into();
    let input_hash = stable_hash(&input_fingerprints(canonical_trace));
    let trace_suffix = short_hash(&input_hash);
    let source_trace_id = format!("{mirror_id}:source:{trace_suffix}");
    let translated_trace_id = format!("{mirror_id}:translated:{trace_suffix}");

    let mut source = TraceBuilder::new(
        source_trace_id,
        source_run_id,
        canonical_trace.initial_viewport,
    )
    .with_metadata("shadow_schema_version", SHADOW_RUN_SCHEMA_VERSION)
    .with_metadata("shadow_mirror_id", mirror_id.as_str())
    .with_metadata("shadow_input_hash", input_hash.as_str())
    .with_metadata("traffic_source_trace_id", canonical_trace.trace_id.as_str());
    let mut translated = TraceBuilder::new(
        translated_trace_id,
        translated_run_id,
        canonical_trace.initial_viewport,
    )
    .with_metadata("shadow_schema_version", SHADOW_RUN_SCHEMA_VERSION)
    .with_metadata("shadow_mirror_id", mirror_id.as_str())
    .with_metadata("shadow_input_hash", input_hash.as_str())
    .with_metadata("traffic_source_trace_id", canonical_trace.trace_id.as_str());

    for event in canonical_trace
        .events
        .iter()
        .filter(|event| is_input_payload(&event.payload))
    {
        source.record_labeled(event.offset_ms, event.payload.clone(), event.label.clone());
        translated.record_labeled(event.offset_ms, event.payload.clone(), event.label.clone());
    }

    ShadowTracePair::new(mirror_id, source.build(), translated.build())
}

fn per_run_breaches(
    pair: &ShadowTracePair,
    oracle_report: &MetamorphicOracleReport,
    thresholds: &ShadowDivergenceThresholds,
) -> Vec<ShadowThresholdBreach> {
    let mut breaches = Vec::new();

    if thresholds.require_mirrored_input_match && !pair.input_evidence.input_hash_matched {
        breaches.push(ShadowThresholdBreach::InputMirrorMismatch {
            source_input_hash: pair.input_evidence.source_input_hash.clone(),
            translated_input_hash: pair.input_evidence.translated_input_hash.clone(),
        });
    }

    if let Err(error) = validate_trace(&pair.source_trace) {
        breaches.push(ShadowThresholdBreach::InvalidTrace {
            side: ShadowTraceSide::Source,
            message: error.to_string(),
        });
    }

    if let Err(error) = validate_trace(&pair.translated_trace) {
        breaches.push(ShadowThresholdBreach::InvalidTrace {
            side: ShadowTraceSide::Translated,
            message: error.to_string(),
        });
    }

    if matches!(oracle_report.verdict, SemanticDiffVerdict::Violation) {
        breaches.push(ShadowThresholdBreach::SemanticViolation {
            violated_relation_ids: oracle_report.violated_relation_ids.clone(),
            violated_clause_ids: oracle_report.strict_report.violated_clause_ids.clone(),
        });

        if oracle_report.strict_report.risk_score > thresholds.max_violation_risk_score {
            breaches.push(ShadowThresholdBreach::ViolationRiskScore {
                risk_score_basis_points: risk_basis_points(oracle_report.strict_report.risk_score),
                max_allowed_basis_points: risk_basis_points(thresholds.max_violation_risk_score),
            });
        }
    }

    breaches
}

fn stability_evidence(observations: &[ShadowRunObservation]) -> ShadowStabilityEvidence {
    let mut stable_mirror_ids = Vec::new();
    let mut violated_mirror_ids = Vec::new();
    let mut input_mismatch_runs = 0_usize;
    let mut current_consecutive_violations = 0_usize;
    let mut max_consecutive_violations = 0_usize;
    let mut total_duration_ms = 0_u64;

    for observation in observations {
        total_duration_ms = total_duration_ms.saturating_add(observation.duration_ms);
        if observation.input_evidence.input_hash_matched {
            if observation.stable {
                stable_mirror_ids.push(observation.mirror_id.clone());
            }
        } else {
            input_mismatch_runs = input_mismatch_runs.saturating_add(1);
        }

        if matches!(
            observation.oracle_report.verdict,
            SemanticDiffVerdict::Violation
        ) {
            violated_mirror_ids.push(observation.mirror_id.clone());
            current_consecutive_violations = current_consecutive_violations.saturating_add(1);
            max_consecutive_violations =
                max_consecutive_violations.max(current_consecutive_violations);
        } else {
            current_consecutive_violations = 0;
        }
    }

    let total_runs = observations.len();
    let violating_runs = violated_mirror_ids.len();
    let violation_rate_per_mille = violating_runs
        .saturating_mul(1000)
        .checked_div(total_runs)
        .unwrap_or(0) as u32;

    ShadowStabilityEvidence {
        total_runs,
        stable_runs: stable_mirror_ids.len(),
        violating_runs,
        input_mismatch_runs,
        total_duration_ms,
        max_consecutive_violations,
        violation_rate_per_mille,
        stable_mirror_ids,
        violated_mirror_ids,
    }
}

fn holdback_reasons(
    observations: &[ShadowRunObservation],
    stability: &ShadowStabilityEvidence,
    config: &ShadowRunConfig,
) -> Vec<ShadowHoldbackReason> {
    let mut reasons = Vec::new();
    let policy = &config.holdback_policy;
    let thresholds = &config.divergence_thresholds;

    if policy.holdback_on_input_mismatch && stability.input_mismatch_runs > 0 {
        reasons.push(reason(
            "shadow.input_mirror_mismatch",
            affected_runs_for(observations, |observation| {
                !observation.input_evidence.input_hash_matched
            }),
            "source and translated traces did not receive identical mirrored input",
        ));
    }

    if policy.holdback_on_invalid_trace {
        let affected = affected_runs_for(observations, |observation| {
            observation
                .threshold_breaches
                .iter()
                .any(|breach| matches!(breach, ShadowThresholdBreach::InvalidTrace { .. }))
        });
        if !affected.is_empty() {
            reasons.push(reason(
                "shadow.invalid_trace",
                affected,
                "one or more shadow traces failed structural validation",
            ));
        }
    }

    if policy.holdback_on_semantic_violation && stability.violating_runs > 0 {
        reasons.push(reason(
            "shadow.semantic_violation",
            stability.violated_mirror_ids.clone(),
            "metamorphic oracle reported semantic violations during shadow execution",
        ));
    }

    if policy.holdback_on_threshold_breach
        && stability.violation_rate_per_mille > thresholds.max_violation_rate_per_mille
    {
        reasons.push(reason(
            "shadow.violation_rate",
            stability.violated_mirror_ids.clone(),
            "shadow violation rate exceeded the explicit threshold",
        ));
    }

    if policy.holdback_on_threshold_breach
        && stability.max_consecutive_violations > thresholds.max_consecutive_violations
    {
        reasons.push(reason(
            "shadow.consecutive_violations",
            stability.violated_mirror_ids.clone(),
            "consecutive shadow violations exceeded the explicit threshold",
        ));
    }

    dedupe_reasons(reasons)
}

fn promotion_ready(stability: &ShadowStabilityEvidence, policy: &ShadowPromotionPolicy) -> bool {
    if stability.total_runs < policy.min_total_runs {
        return false;
    }
    if stability.stable_runs < policy.required_stable_runs {
        return false;
    }
    if stability.total_duration_ms < policy.required_stable_duration_ms {
        return false;
    }
    if policy.require_all_inputs_mirrored && stability.input_mismatch_runs > 0 {
        return false;
    }
    if policy.require_no_holdbacks
        && (stability.violating_runs > 0 || stability.max_consecutive_violations > 0)
    {
        return false;
    }
    true
}

fn affected_runs_for(
    observations: &[ShadowRunObservation],
    predicate: impl Fn(&ShadowRunObservation) -> bool,
) -> Vec<String> {
    observations
        .iter()
        .filter(|observation| predicate(observation))
        .map(|observation| observation.mirror_id.clone())
        .collect()
}

fn reason(
    reason_id: impl Into<String>,
    affected_mirror_ids: Vec<String>,
    message: impl Into<String>,
) -> ShadowHoldbackReason {
    ShadowHoldbackReason {
        reason_id: reason_id.into(),
        affected_mirror_ids,
        message: message.into(),
    }
}

fn dedupe_reasons(reasons: Vec<ShadowHoldbackReason>) -> Vec<ShadowHoldbackReason> {
    let mut seen = BTreeSet::new();
    reasons
        .into_iter()
        .filter(|reason| {
            seen.insert(format!(
                "{}:{}",
                reason.reason_id,
                reason.affected_mirror_ids.join(",")
            ))
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
struct InputFingerprint {
    offset_ms: u64,
    label: Option<String>,
    payload: TracePayload,
}

fn input_fingerprints(trace: &InteractionTrace) -> Vec<InputFingerprint> {
    trace
        .events
        .iter()
        .filter(|event| is_input_payload(&event.payload))
        .map(|event| InputFingerprint {
            offset_ms: event.offset_ms,
            label: event.label.clone(),
            payload: event.payload.clone(),
        })
        .collect()
}

fn is_input_payload(payload: &TracePayload) -> bool {
    matches!(
        payload,
        TracePayload::Key { .. }
            | TracePayload::TextInput { .. }
            | TracePayload::Mouse { .. }
            | TracePayload::Scroll { .. }
            | TracePayload::Resize { .. }
    )
}

fn report_evidence_checksum(
    observations: &[ShadowRunObservation],
    stability: &ShadowStabilityEvidence,
) -> String {
    #[derive(Serialize)]
    struct ObservationSummary<'a> {
        mirror_id: &'a str,
        source_trace_id: &'a str,
        translated_trace_id: &'a str,
        input_hash: &'a str,
        verdict: SemanticDiffVerdict,
        stable: bool,
        breaches: &'a [ShadowThresholdBreach],
    }

    #[derive(Serialize)]
    struct EvidenceSummary<'a> {
        observations: Vec<ObservationSummary<'a>>,
        stability: &'a ShadowStabilityEvidence,
    }

    let summaries = observations
        .iter()
        .map(|observation| ObservationSummary {
            mirror_id: observation.mirror_id.as_str(),
            source_trace_id: observation.source_trace_id.as_str(),
            translated_trace_id: observation.translated_trace_id.as_str(),
            input_hash: observation.input_evidence.source_input_hash.as_str(),
            verdict: observation.oracle_report.verdict,
            stable: observation.stable,
            breaches: &observation.threshold_breaches,
        })
        .collect::<Vec<_>>();
    stable_hash(&EvidenceSummary {
        observations: summaries,
        stability,
    })
}

fn stable_hash<T: Serialize>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

fn risk_basis_points(value: f64) -> u32 {
    let clamped = value.clamp(0.0, 1.0);
    (clamped * 10_000.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{KeyAction, TracePayload, Viewport};

    fn viewport() -> Viewport {
        Viewport {
            width: 80,
            height: 24,
        }
    }

    fn canonical_input_trace() -> InteractionTrace {
        let mut builder = TraceBuilder::new("traffic", "traffic-run", viewport());
        builder.record(
            0,
            TracePayload::Key {
                key: "j".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
        builder.record(
            5,
            TracePayload::Resize {
                width: 100,
                height: 30,
            },
        );
        builder.record(
            8,
            TracePayload::StateCapture {
                state_hash: "ignored-output".to_string(),
                component: Some("source-only".to_string()),
            },
        );
        builder.build()
    }

    fn trace_with_markers(
        trace_id: &str,
        run_id: &str,
        marker_names: &[&str],
        final_state: &str,
    ) -> InteractionTrace {
        let mut builder = TraceBuilder::new(trace_id, run_id, viewport());
        builder.record(
            0,
            TracePayload::Key {
                key: "enter".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
        for (index, name) in marker_names.iter().enumerate() {
            builder.record(
                10 + index as u64,
                TracePayload::Marker {
                    name: (*name).to_string(),
                },
            );
        }
        builder.record(
            30,
            TracePayload::StateCapture {
                state_hash: final_state.to_string(),
                component: Some("app".to_string()),
            },
        );
        builder.build()
    }

    fn stable_pair(mirror_id: &str) -> ShadowTracePair {
        ShadowTracePair::new(
            mirror_id,
            trace_with_markers(
                &format!("{mirror_id}:source"),
                &format!("{mirror_id}:source-run"),
                &["event:toast=queued", "event:toast=shown"],
                "state-ready",
            ),
            trace_with_markers(
                &format!("{mirror_id}:translated"),
                &format!("{mirror_id}:translated-run"),
                &["event:toast=shown", "event:toast=queued"],
                "state-ready",
            ),
        )
    }

    #[test]
    fn mirrored_input_trace_has_deterministic_capture_hash() {
        let pair = mirror_input_trace(
            &canonical_input_trace(),
            "mirror-1",
            "source-run",
            "translated-run",
        );

        assert!(validate_trace(&pair.source_trace).is_ok());
        assert!(validate_trace(&pair.translated_trace).is_ok());
        assert!(pair.input_evidence.input_hash_matched);
        assert_eq!(pair.input_evidence.source_input_events, 2);
        assert_eq!(pair.input_evidence.translated_input_events, 2);
        assert!(
            pair.input_evidence
                .source_input_hash
                .as_str()
                .cmp(pair.input_evidence.translated_input_hash.as_str())
                .is_eq()
        );
        assert!(
            pair.source_trace
                .metadata
                .get("shadow_mirror_id")
                .is_some_and(|value| value == "mirror-1")
        );
    }

    #[test]
    fn input_mirror_mismatch_forces_holdback() {
        let source = trace_with_markers("source", "source-run", &["event:source=only"], "state-a");
        let mut translated = trace_with_markers(
            "translated",
            "translated-run",
            &["event:source=only"],
            "state-a",
        );
        if let Some(event) = translated.events.first_mut() {
            event.payload = TracePayload::Key {
                key: "escape".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            };
            translated.events_hash = stable_hash(&translated.events);
        }

        let harness = ShadowRunHarness::new(ShadowRunConfig::smoke_test());
        let report = harness.evaluate_pairs(vec![ShadowTracePair::new(
            "mirror-mismatch",
            source,
            translated,
        )]);

        assert!(matches!(report.decision, ShadowPromotionDecision::Holdback));
        assert!(
            report
                .holdback_reasons
                .iter()
                .any(|reason| reason.reason_id == "shadow.input_mirror_mismatch")
        );
    }

    #[test]
    fn schedule_tolerant_shadow_run_accepts_reordered_output_events() {
        let harness = ShadowRunHarness::new(ShadowRunConfig::smoke_test());
        let report = harness.evaluate_pairs(vec![stable_pair("mirror-stable")]);

        assert!(matches!(report.decision, ShadowPromotionDecision::Promote));
        assert_eq!(report.stability_evidence.stable_runs, 1);
        assert_eq!(report.stability_evidence.violating_runs, 0);
        assert!(report.holdback_reasons.is_empty());
    }

    #[test]
    fn promotion_requires_sustained_stability_window() {
        let harness = ShadowRunHarness::production_default();
        let short_report = harness.evaluate_pairs(vec![stable_pair("mirror-short")]);
        assert!(matches!(
            short_report.decision,
            ShadowPromotionDecision::ContinueShadow
        ));

        let long_report = harness.evaluate_pairs(vec![
            stable_pair("mirror-a"),
            stable_pair("mirror-b"),
            stable_pair("mirror-c"),
        ]);
        assert_eq!(long_report.stability_evidence.stable_runs, 3);
        assert!(matches!(
            long_report.decision,
            ShadowPromotionDecision::ContinueShadow
        ));

        let mut config = ShadowRunConfig::production_default();
        config.promotion_policy.required_stable_duration_ms = 0;
        let lenient_harness = ShadowRunHarness::new(config);
        let promoted = lenient_harness.evaluate_pairs(vec![
            stable_pair("mirror-a"),
            stable_pair("mirror-b"),
            stable_pair("mirror-c"),
        ]);
        assert!(matches!(
            promoted.decision,
            ShadowPromotionDecision::Promote
        ));
    }

    #[test]
    fn semantic_violation_threshold_triggers_holdback() {
        let source = trace_with_markers("source", "source-run", &["event:save=ok"], "state-ok");
        let translated = trace_with_markers(
            "translated",
            "translated-run",
            &["event:save=ok"],
            "state-bad",
        );
        let harness = ShadowRunHarness::new(ShadowRunConfig::strict());

        let report =
            harness.evaluate_pairs(vec![ShadowTracePair::new("mirror-bad", source, translated)]);

        assert!(matches!(report.decision, ShadowPromotionDecision::Holdback));
        assert_eq!(report.stability_evidence.violating_runs, 1);
        assert!(report.observations.first().is_some_and(|observation| {
            observation
                .threshold_breaches
                .iter()
                .any(|breach| matches!(breach, ShadowThresholdBreach::SemanticViolation { .. }))
        }));
    }
}
