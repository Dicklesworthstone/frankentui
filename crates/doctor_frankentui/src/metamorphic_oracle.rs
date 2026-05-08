//! Metamorphic relation oracle for non-deterministic equivalence checks.
//!
//! Strict semantic comparison is still the default. This module adds an
//! explicit policy layer for migration scenarios where schedule or timing
//! variation makes exact observation order brittle, while relation-level
//! witnesses keep every accepted or rejected difference auditable.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic_diff::{
    CounterexampleTrace, SemanticDiffReport, SemanticDiffVerdict, SemanticDifferenceKind,
    SemanticObservation, SemanticObservationKind, SemanticRun, compare_runs, compare_traces,
};
use crate::trace::InteractionTrace;

/// Schema version for metamorphic reports and relation descriptors.
pub const METAMORPHIC_ORACLE_VERSION: &str = "metamorphic-oracle-v1";
/// Version for the built-in relation catalog.
pub const METAMORPHIC_RELATION_CATALOG_VERSION: &str = "metamorphic-relation-catalog-v1";

/// Policy profile controlling how strict and metamorphic evidence compose.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetamorphicPolicyProfile {
    /// Use the strict semantic comparator only.
    Strict,
    /// Accept deterministic multiset/final-state equivalence under scheduling drift.
    ScheduleTolerant,
    /// Apply every built-in relation intended for non-deterministic equivalence.
    NondeterministicEquivalence,
}

/// Feature family covered by a metamorphic relation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MetamorphicFeatureFamily {
    EventOrdering,
    StateTransition,
    SideEffect,
    ImprovementEnvelope,
}

impl MetamorphicFeatureFamily {
    fn catalog_version(self) -> String {
        let family = match self {
            Self::EventOrdering => "event-ordering",
            Self::StateTransition => "state-transition",
            Self::SideEffect => "side-effect",
            Self::ImprovementEnvelope => "improvement-envelope",
        };
        format!("{METAMORPHIC_RELATION_CATALOG_VERSION}:{family}")
    }
}

/// Built-in relation kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetamorphicRelationKind {
    /// Same observation multiset regardless of order or offset.
    MultisetEquivalence,
    /// Same final value per state key.
    FinalValueEquivalence,
    /// Same side-effect multiset regardless of schedule.
    RequiredSetEquivalence,
    /// No forbidden improvement clause appears.
    ImprovementEnvelopePreserved,
}

/// Versioned descriptor for one relation in the built-in catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetamorphicRelationDescriptor {
    pub relation_id: String,
    pub catalog_version: String,
    pub feature_family: MetamorphicFeatureFamily,
    pub family_catalog_version: String,
    pub relation_kind: MetamorphicRelationKind,
    pub policy_profiles: Vec<MetamorphicPolicyProfile>,
    pub description: String,
    pub proof_obligation: String,
}

/// Comparator configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetamorphicOracleConfig {
    pub profile: MetamorphicPolicyProfile,
    pub catalog_version: String,
    pub enabled_relation_ids: Vec<String>,
}

impl MetamorphicOracleConfig {
    /// Strict semantic comparison only.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            profile: MetamorphicPolicyProfile::Strict,
            catalog_version: METAMORPHIC_RELATION_CATALOG_VERSION.to_string(),
            enabled_relation_ids: Vec::new(),
        }
    }

    /// Built-in schedule-tolerant profile.
    #[must_use]
    pub fn schedule_tolerant() -> Self {
        Self::for_profile(MetamorphicPolicyProfile::ScheduleTolerant)
    }

    /// Built-in non-deterministic equivalence profile.
    #[must_use]
    pub fn nondeterministic_equivalence() -> Self {
        Self::for_profile(MetamorphicPolicyProfile::NondeterministicEquivalence)
    }

    /// Create a config for a built-in profile.
    #[must_use]
    pub fn for_profile(profile: MetamorphicPolicyProfile) -> Self {
        let enabled_relation_ids = metamorphic_relation_catalog()
            .into_iter()
            .filter(|relation| relation.policy_profiles.contains(&profile))
            .map(|relation| relation.relation_id)
            .collect();
        Self {
            profile,
            catalog_version: METAMORPHIC_RELATION_CATALOG_VERSION.to_string(),
            enabled_relation_ids,
        }
    }

    fn relation_enabled(&self, relation_id: &str) -> bool {
        self.enabled_relation_ids
            .iter()
            .any(|enabled| enabled.as_str().cmp(relation_id).is_eq())
    }
}

impl Default for MetamorphicOracleConfig {
    fn default() -> Self {
        Self::strict()
    }
}

/// Minimal witness emitted when a relation fails.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetamorphicWitness {
    pub witness_id: String,
    pub relation_id: String,
    pub feature_family: MetamorphicFeatureFamily,
    pub source_observations: Vec<SemanticObservation>,
    pub translated_observations: Vec<SemanticObservation>,
    pub source_signature: Option<String>,
    pub translated_signature: Option<String>,
    pub message: String,
}

/// Per-relation evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetamorphicRelationResult {
    pub relation_id: String,
    pub feature_family: MetamorphicFeatureFamily,
    pub verdict: SemanticDiffVerdict,
    pub observations_compared: usize,
    pub witness: Option<MetamorphicWitness>,
}

/// Full strict-plus-metamorphic comparison report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetamorphicOracleReport {
    pub oracle_id: String,
    pub catalog_version: String,
    pub profile: MetamorphicPolicyProfile,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub strict_report: SemanticDiffReport,
    pub verdict: SemanticDiffVerdict,
    pub relation_results: Vec<MetamorphicRelationResult>,
    pub witness_set: Vec<MetamorphicWitness>,
    pub covered_relation_ids: Vec<String>,
    pub violated_relation_ids: Vec<String>,
    pub counterexample: Option<CounterexampleTrace>,
}

/// Return the built-in versioned metamorphic relation catalog.
#[must_use]
pub fn metamorphic_relation_catalog() -> Vec<MetamorphicRelationDescriptor> {
    vec![
        relation_descriptor(
            "mr:event_ordering:multiset-equivalence",
            MetamorphicFeatureFamily::EventOrdering,
            MetamorphicRelationKind::MultisetEquivalence,
            "Event-ordering observations may move in time, but their semantic multiset must match.",
            "For every event-ordering signature s, source and translated counts must match.",
        ),
        relation_descriptor(
            "mr:state_transition:final-value-equivalence",
            MetamorphicFeatureFamily::StateTransition,
            MetamorphicRelationKind::FinalValueEquivalence,
            "Intermediate state snapshots may differ under scheduling drift, but final state per key must match.",
            "For every state key k, source and translated final values must match.",
        ),
        relation_descriptor(
            "mr:side_effect:required-set-equivalence",
            MetamorphicFeatureFamily::SideEffect,
            MetamorphicRelationKind::RequiredSetEquivalence,
            "Side effects may be reordered, but required side-effect observations must not be lost or invented.",
            "For every side-effect signature s, source and translated counts must match.",
        ),
        relation_descriptor(
            "mr:improvement:envelope-preserved",
            MetamorphicFeatureFamily::ImprovementEnvelope,
            MetamorphicRelationKind::ImprovementEnvelopePreserved,
            "Metamorphic acceptance cannot launder forbidden improvement claims.",
            "The strict report must contain no ForbiddenImprovement semantic difference.",
        ),
    ]
}

/// Compare traces under the requested strict/metamorphic profile.
#[must_use]
pub fn compare_traces_with_metamorphic_oracle(
    source_trace: &InteractionTrace,
    translated_trace: &InteractionTrace,
    config: &MetamorphicOracleConfig,
) -> MetamorphicOracleReport {
    let strict_report = compare_traces(source_trace, translated_trace);
    let source_run = SemanticRun::from_trace(source_trace);
    let translated_run = SemanticRun::from_trace(translated_trace);
    compare_runs_with_metamorphic_oracle_and_report(
        &source_run,
        &translated_run,
        strict_report,
        config,
    )
}

/// Compare semantic runs under the requested strict/metamorphic profile.
#[must_use]
pub fn compare_runs_with_metamorphic_oracle(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    config: &MetamorphicOracleConfig,
) -> MetamorphicOracleReport {
    let strict_report = compare_runs(source_run, translated_run);
    compare_runs_with_metamorphic_oracle_and_report(
        source_run,
        translated_run,
        strict_report,
        config,
    )
}

fn compare_runs_with_metamorphic_oracle_and_report(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    strict_report: SemanticDiffReport,
    config: &MetamorphicOracleConfig,
) -> MetamorphicOracleReport {
    if config.profile == MetamorphicPolicyProfile::Strict {
        return report_from_parts(
            source_run,
            translated_run,
            strict_report,
            config,
            Vec::new(),
        );
    }

    let relation_results = metamorphic_relation_catalog()
        .into_iter()
        .filter(|relation| config.relation_enabled(&relation.relation_id))
        .map(|relation| evaluate_relation(&relation, source_run, translated_run, &strict_report))
        .collect::<Vec<_>>();

    report_from_parts(
        source_run,
        translated_run,
        strict_report,
        config,
        relation_results,
    )
}

fn report_from_parts(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    strict_report: SemanticDiffReport,
    config: &MetamorphicOracleConfig,
    relation_results: Vec<MetamorphicRelationResult>,
) -> MetamorphicOracleReport {
    let violated_relation_ids = relation_results
        .iter()
        .filter(|result| result.verdict == SemanticDiffVerdict::Violation)
        .map(|result| result.relation_id.clone())
        .collect::<Vec<_>>();
    let covered_relation_ids = relation_results
        .iter()
        .filter(|result| result.verdict != SemanticDiffVerdict::Violation)
        .map(|result| result.relation_id.clone())
        .collect::<Vec<_>>();
    let witness_set = relation_results
        .iter()
        .filter_map(|result| result.witness.clone())
        .collect::<Vec<_>>();
    let verdict = if config.profile == MetamorphicPolicyProfile::Strict
        || strict_report.verdict != SemanticDiffVerdict::Violation
    {
        strict_report.verdict
    } else if violated_relation_ids.is_empty() {
        SemanticDiffVerdict::Equivalent
    } else {
        SemanticDiffVerdict::Violation
    };
    let counterexample = if verdict == SemanticDiffVerdict::Violation {
        strict_report.counterexample.clone()
    } else {
        None
    };

    MetamorphicOracleReport {
        oracle_id: METAMORPHIC_ORACLE_VERSION.to_string(),
        catalog_version: config.catalog_version.clone(),
        profile: config.profile,
        source_run_id: source_run.run_id.clone(),
        translated_run_id: translated_run.run_id.clone(),
        strict_report,
        verdict,
        relation_results,
        witness_set,
        covered_relation_ids,
        violated_relation_ids,
        counterexample,
    }
}

fn evaluate_relation(
    descriptor: &MetamorphicRelationDescriptor,
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    strict_report: &SemanticDiffReport,
) -> MetamorphicRelationResult {
    match descriptor.relation_kind {
        MetamorphicRelationKind::MultisetEquivalence => multiset_relation(
            descriptor,
            source_run,
            translated_run,
            SemanticObservationKind::EventOrdering,
        ),
        MetamorphicRelationKind::FinalValueEquivalence => {
            final_value_relation(descriptor, source_run, translated_run)
        }
        MetamorphicRelationKind::RequiredSetEquivalence => multiset_relation(
            descriptor,
            source_run,
            translated_run,
            SemanticObservationKind::SideEffect,
        ),
        MetamorphicRelationKind::ImprovementEnvelopePreserved => {
            improvement_envelope_relation(descriptor, strict_report)
        }
    }
}

fn multiset_relation(
    descriptor: &MetamorphicRelationDescriptor,
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    kind: SemanticObservationKind,
) -> MetamorphicRelationResult {
    let source = observations_by_kind(&source_run.observations, kind);
    let translated = observations_by_kind(&translated_run.observations, kind);
    let source_counts = signature_counts(&source);
    let translated_counts = signature_counts(&translated);
    let observations_compared = source.len().max(translated.len());

    if counts_match(&source_counts, &translated_counts) {
        return passing_result(descriptor, observations_compared);
    }

    let mismatch = first_count_mismatch(&source_counts, &translated_counts);
    let witness = mismatch.map(|signature| {
        witness_for_signature(
            descriptor,
            &source,
            &translated,
            &signature,
            format!(
                "relation {} failed: source and translated multisets differ for signature '{}'",
                descriptor.relation_id, signature
            ),
        )
    });
    failing_result(descriptor, observations_compared, witness)
}

fn final_value_relation(
    descriptor: &MetamorphicRelationDescriptor,
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
) -> MetamorphicRelationResult {
    let source = observations_by_kind(
        &source_run.observations,
        SemanticObservationKind::StateTransition,
    );
    let translated = observations_by_kind(
        &translated_run.observations,
        SemanticObservationKind::StateTransition,
    );
    let source_final = final_values_by_key(&source);
    let translated_final = final_values_by_key(&translated);
    let observations_compared = source.len().max(translated.len());

    if values_match(&source_final, &translated_final) {
        return passing_result(descriptor, observations_compared);
    }

    let mismatch_key = first_key_mismatch(&source_final, &translated_final);
    let witness = mismatch_key.map(|key| {
        witness_for_key(
            descriptor,
            &source,
            &translated,
            &key,
            format!(
                "relation {} failed: final state value diverged for key '{}'",
                descriptor.relation_id, key
            ),
        )
    });
    failing_result(descriptor, observations_compared, witness)
}

fn improvement_envelope_relation(
    descriptor: &MetamorphicRelationDescriptor,
    strict_report: &SemanticDiffReport,
) -> MetamorphicRelationResult {
    let forbidden = strict_report.differences.iter().find(|difference| {
        difference.difference_kind == SemanticDifferenceKind::ForbiddenImprovement
    });

    let Some(forbidden) = forbidden else {
        return passing_result(descriptor, strict_report.differences.len());
    };

    let source_signature = forbidden.source_value.clone();
    let translated_signature = forbidden.translated_value.clone();
    let witness = MetamorphicWitness {
        witness_id: witness_id(
            &descriptor.relation_id,
            source_signature.as_deref(),
            translated_signature.as_deref(),
        ),
        relation_id: descriptor.relation_id.clone(),
        feature_family: descriptor.feature_family,
        source_observations: Vec::new(),
        translated_observations: Vec::new(),
        source_signature,
        translated_signature,
        message: format!(
            "relation {} failed: forbidden improvement '{}' is outside the declared envelope",
            descriptor.relation_id, forbidden.key
        ),
    };
    failing_result(descriptor, strict_report.differences.len(), Some(witness))
}

fn relation_descriptor(
    relation_id: &str,
    feature_family: MetamorphicFeatureFamily,
    relation_kind: MetamorphicRelationKind,
    description: &str,
    proof_obligation: &str,
) -> MetamorphicRelationDescriptor {
    MetamorphicRelationDescriptor {
        relation_id: relation_id.to_string(),
        catalog_version: METAMORPHIC_RELATION_CATALOG_VERSION.to_string(),
        feature_family,
        family_catalog_version: feature_family.catalog_version(),
        relation_kind,
        policy_profiles: vec![
            MetamorphicPolicyProfile::ScheduleTolerant,
            MetamorphicPolicyProfile::NondeterministicEquivalence,
        ],
        description: description.to_string(),
        proof_obligation: proof_obligation.to_string(),
    }
}

fn passing_result(
    descriptor: &MetamorphicRelationDescriptor,
    observations_compared: usize,
) -> MetamorphicRelationResult {
    MetamorphicRelationResult {
        relation_id: descriptor.relation_id.clone(),
        feature_family: descriptor.feature_family,
        verdict: SemanticDiffVerdict::Equivalent,
        observations_compared,
        witness: None,
    }
}

fn failing_result(
    descriptor: &MetamorphicRelationDescriptor,
    observations_compared: usize,
    witness: Option<MetamorphicWitness>,
) -> MetamorphicRelationResult {
    MetamorphicRelationResult {
        relation_id: descriptor.relation_id.clone(),
        feature_family: descriptor.feature_family,
        verdict: SemanticDiffVerdict::Violation,
        observations_compared,
        witness,
    }
}

fn observations_by_kind(
    observations: &[SemanticObservation],
    kind: SemanticObservationKind,
) -> Vec<SemanticObservation> {
    let mut selected = observations
        .iter()
        .filter(|observation| observation.kind == kind)
        .cloned()
        .collect::<Vec<_>>();
    selected.sort_by(|a, b| {
        a.sequence
            .cmp(&b.sequence)
            .then_with(|| a.offset_ms.cmp(&b.offset_ms))
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.value.cmp(&b.value))
    });
    selected
}

fn signature_counts(observations: &[SemanticObservation]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for observation in observations {
        let entry = counts
            .entry(observation.comparable_signature())
            .or_insert(0_usize);
        *entry = entry.saturating_add(1);
    }
    counts
}

fn final_values_by_key(observations: &[SemanticObservation]) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for observation in observations {
        values.insert(observation.key.clone(), observation.value.clone());
    }
    values
}

fn first_count_mismatch(
    source_counts: &BTreeMap<String, usize>,
    translated_counts: &BTreeMap<String, usize>,
) -> Option<String> {
    source_counts
        .keys()
        .chain(translated_counts.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .find(|signature| {
            !optional_values_match(
                source_counts.get(*signature),
                translated_counts.get(*signature),
            )
        })
        .cloned()
}

fn first_key_mismatch(
    source_values: &BTreeMap<String, String>,
    translated_values: &BTreeMap<String, String>,
) -> Option<String> {
    source_values
        .keys()
        .chain(translated_values.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .find(|key| !optional_values_match(source_values.get(*key), translated_values.get(*key)))
        .cloned()
}

fn counts_match(
    source_counts: &BTreeMap<String, usize>,
    translated_counts: &BTreeMap<String, usize>,
) -> bool {
    source_counts.len() == translated_counts.len()
        && source_counts.iter().all(|(signature, source_count)| {
            translated_counts
                .get(signature)
                .is_some_and(|translated_count| source_count.cmp(translated_count).is_eq())
        })
}

fn values_match(
    source_values: &BTreeMap<String, String>,
    translated_values: &BTreeMap<String, String>,
) -> bool {
    source_values.len() == translated_values.len()
        && source_values.iter().all(|(key, source_value)| {
            translated_values
                .get(key)
                .is_some_and(|translated_value| source_value.as_str().cmp(translated_value).is_eq())
        })
}

fn optional_values_match<T: Ord>(source: Option<&T>, translated: Option<&T>) -> bool {
    match (source, translated) {
        (Some(source), Some(translated)) => source.cmp(translated).is_eq(),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

fn witness_for_signature(
    descriptor: &MetamorphicRelationDescriptor,
    source: &[SemanticObservation],
    translated: &[SemanticObservation],
    signature: &str,
    message: String,
) -> MetamorphicWitness {
    let source_observations = first_matching_signature(source, signature);
    let translated_observations = first_matching_signature(translated, signature);
    MetamorphicWitness {
        witness_id: witness_id(&descriptor.relation_id, Some(signature), Some(signature)),
        relation_id: descriptor.relation_id.clone(),
        feature_family: descriptor.feature_family,
        source_observations,
        translated_observations,
        source_signature: Some(signature.to_string()),
        translated_signature: Some(signature.to_string()),
        message,
    }
}

fn witness_for_key(
    descriptor: &MetamorphicRelationDescriptor,
    source: &[SemanticObservation],
    translated: &[SemanticObservation],
    key: &str,
    message: String,
) -> MetamorphicWitness {
    let source_observations = first_matching_key(source, key);
    let translated_observations = first_matching_key(translated, key);
    let source_signature = source_observations
        .first()
        .map(SemanticObservation::comparable_signature);
    let translated_signature = translated_observations
        .first()
        .map(SemanticObservation::comparable_signature);
    MetamorphicWitness {
        witness_id: witness_id(
            &descriptor.relation_id,
            source_signature.as_deref(),
            translated_signature.as_deref(),
        ),
        relation_id: descriptor.relation_id.clone(),
        feature_family: descriptor.feature_family,
        source_observations,
        translated_observations,
        source_signature,
        translated_signature,
        message,
    }
}

fn first_matching_signature(
    observations: &[SemanticObservation],
    signature: &str,
) -> Vec<SemanticObservation> {
    observations
        .iter()
        .find(|observation| {
            observation
                .comparable_signature()
                .as_str()
                .cmp(signature)
                .is_eq()
        })
        .cloned()
        .into_iter()
        .collect()
}

fn first_matching_key(observations: &[SemanticObservation], key: &str) -> Vec<SemanticObservation> {
    observations
        .iter()
        .rev()
        .find(|observation| observation.key.as_str().cmp(key).is_eq())
        .cloned()
        .into_iter()
        .collect()
}

fn witness_id(
    relation_id: &str,
    source_signature: Option<&str>,
    translated_signature: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(METAMORPHIC_ORACLE_VERSION.as_bytes());
    hasher.update(relation_id.as_bytes());
    hasher.update(source_signature.unwrap_or("source:none").as_bytes());
    hasher.update(translated_signature.unwrap_or("translated:none").as_bytes());
    format!("mr-witness:{}", hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{KeyAction, TraceBuilder, TracePayload, Viewport};

    fn viewport() -> Viewport {
        Viewport {
            width: 80,
            height: 24,
        }
    }

    fn event_trace(trace_id: &str, run_id: &str, keys: &[&str]) -> InteractionTrace {
        let mut builder = TraceBuilder::new(trace_id, run_id, viewport()).with_metadata(
            "replay_command",
            format!("doctor_frankentui replay {trace_id}"),
        );
        for (index, key) in keys.iter().enumerate() {
            builder.record(
                u64::try_from(index).unwrap_or(u64::MAX),
                TracePayload::Key {
                    key: (*key).to_string(),
                    modifiers: Vec::new(),
                    action: KeyAction::Press,
                },
            );
        }
        builder.build()
    }

    fn state_trace(trace_id: &str, run_id: &str, states: &[&str]) -> InteractionTrace {
        let mut builder = TraceBuilder::new(trace_id, run_id, viewport());
        for (index, state) in states.iter().enumerate() {
            builder.record(
                u64::try_from(index).unwrap_or(u64::MAX),
                TracePayload::StateCapture {
                    state_hash: (*state).to_string(),
                    component: Some("counter".to_string()),
                },
            );
        }
        builder.build()
    }

    fn effect_trace(trace_id: &str, run_id: &str, effects: &[&str]) -> InteractionTrace {
        let mut builder = TraceBuilder::new(trace_id, run_id, viewport());
        for (index, effect) in effects.iter().enumerate() {
            builder.record(
                u64::try_from(index).unwrap_or(u64::MAX),
                TracePayload::Marker {
                    name: format!("effect:{effect}=observed"),
                },
            );
        }
        builder.build()
    }

    #[test]
    fn catalog_is_versioned_by_feature_family() {
        let catalog = metamorphic_relation_catalog();
        assert_eq!(catalog.len(), 4);
        assert!(catalog.iter().all(|relation| {
            relation
                .catalog_version
                .as_str()
                .cmp(METAMORPHIC_RELATION_CATALOG_VERSION)
                .is_eq()
                && relation
                    .family_catalog_version
                    .starts_with(METAMORPHIC_RELATION_CATALOG_VERSION)
                && !relation.relation_id.is_empty()
                && relation
                    .policy_profiles
                    .contains(&MetamorphicPolicyProfile::ScheduleTolerant)
        }));
    }

    #[test]
    fn strict_profile_preserves_semantic_diff_violation() {
        let source = event_trace("source", "source-run", &["a", "b"]);
        let translated = event_trace("translated", "translated-run", &["b", "a"]);

        let report = compare_traces_with_metamorphic_oracle(
            &source,
            &translated,
            &MetamorphicOracleConfig::strict(),
        );

        assert_eq!(report.verdict, SemanticDiffVerdict::Violation);
        assert_eq!(report.strict_report.verdict, SemanticDiffVerdict::Violation);
        assert!(report.relation_results.is_empty());
    }

    #[test]
    fn schedule_tolerant_profile_accepts_reordered_equivalent_events() {
        let source = event_trace("source", "source-run", &["a", "b", "c"]);
        let translated = event_trace("translated", "translated-run", &["c", "a", "b"]);

        let report = compare_traces_with_metamorphic_oracle(
            &source,
            &translated,
            &MetamorphicOracleConfig::schedule_tolerant(),
        );

        assert_eq!(report.strict_report.verdict, SemanticDiffVerdict::Violation);
        assert_eq!(report.verdict, SemanticDiffVerdict::Equivalent);
        assert!(report.witness_set.is_empty());
        assert!(
            report
                .covered_relation_ids
                .contains(&"mr:event_ordering:multiset-equivalence".to_string())
        );
    }

    #[test]
    fn metamorphic_violation_emits_minimal_relation_witness() {
        let source = event_trace("source", "source-run", &["a", "b"]);
        let translated = event_trace("translated", "translated-run", &["a", "x"]);

        let report = compare_traces_with_metamorphic_oracle(
            &source,
            &translated,
            &MetamorphicOracleConfig::schedule_tolerant(),
        );

        assert_eq!(report.verdict, SemanticDiffVerdict::Violation);
        assert_eq!(
            report.violated_relation_ids,
            vec!["mr:event_ordering:multiset-equivalence".to_string()]
        );
        assert_eq!(report.witness_set.len(), 1);
        let Some(witness) = report.witness_set.first() else {
            return;
        };
        assert_eq!(
            witness.relation_id,
            "mr:event_ordering:multiset-equivalence"
        );
        assert!(witness.source_observations.len() <= 1);
        assert!(witness.translated_observations.len() <= 1);
        assert!(witness.witness_id.starts_with("mr-witness:"));
    }

    #[test]
    fn state_final_value_relation_tolerates_intermediate_variation() {
        let source = state_trace("source", "source-run", &["loading", "ready"]);
        let translated = state_trace("translated", "translated-run", &["ready"]);

        let report = compare_traces_with_metamorphic_oracle(
            &source,
            &translated,
            &MetamorphicOracleConfig::nondeterministic_equivalence(),
        );

        assert_eq!(report.strict_report.verdict, SemanticDiffVerdict::Violation);
        assert_eq!(report.verdict, SemanticDiffVerdict::Equivalent);
        assert!(
            report
                .covered_relation_ids
                .contains(&"mr:state_transition:final-value-equivalence".to_string())
        );
    }

    #[test]
    fn side_effect_multiset_relation_rejects_missing_effect() {
        let source = effect_trace("source", "source-run", &["network.fetch"]);
        let translated = effect_trace("translated", "translated-run", &[]);

        let report = compare_traces_with_metamorphic_oracle(
            &source,
            &translated,
            &MetamorphicOracleConfig::schedule_tolerant(),
        );

        assert_eq!(report.verdict, SemanticDiffVerdict::Violation);
        assert!(
            report
                .violated_relation_ids
                .contains(&"mr:side_effect:required-set-equivalence".to_string())
        );
        assert!(report.witness_set.iter().any(|witness| {
            witness
                .relation_id
                .as_str()
                .cmp("mr:side_effect:required-set-equivalence")
                .is_eq()
                && witness.source_observations.len() == 1
                && witness.translated_observations.is_empty()
        }));
    }
}
