//! Concolic-style differential exploration over interaction traces.
//!
//! This module stays deliberately pure: it mutates and minimizes canonical
//! [`InteractionTrace`](crate::trace::InteractionTrace) pairs, then delegates
//! behavioral comparison to [`semantic_diff`](crate::semantic_diff). Runtime
//! harnesses can use the emitted fixture payloads to replay discovered
//! counterexamples against real source and translated applications.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic_diff::{
    CounterexampleTrace, SemanticDiffReport, SemanticDiffVerdict, SemanticDifference,
    compare_traces,
};
use crate::trace::{
    InteractionTrace, KeyAction, MouseAction, TraceBuilder, TraceEvent, TracePayload,
};

/// Version tag for concolic differential reports and exported fixtures.
pub const CONCOLIC_DIFFERENTIAL_VERSION: &str = "concolic-differential-v1";

/// Bounded, deterministic resource policy for concolic exploration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConcolicBudget {
    /// Deterministic seed used to order mutation candidates.
    pub seed: u64,
    /// Maximum candidates to evaluate.
    pub max_candidates: usize,
    /// Maximum number of events included in generated prefix/window candidates.
    pub max_events_per_candidate: usize,
    /// Maximum payload mutation candidates generated after structural candidates.
    pub max_payload_mutations: usize,
    /// Maximum greedy shrink passes once a divergent candidate is found.
    pub max_shrink_passes: usize,
}

impl Default for ConcolicBudget {
    fn default() -> Self {
        Self {
            seed: 0xC0DE_C0DE,
            max_candidates: 128,
            max_events_per_candidate: 64,
            max_payload_mutations: 32,
            max_shrink_passes: 8,
        }
    }
}

/// Trace mutation strategy used to construct a candidate pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceMutation {
    /// Compare the original trace pair.
    Baseline,
    /// Compare matching prefixes from both traces.
    Prefix { len: usize },
    /// Compare one matching event index from both traces.
    Singleton { index: usize },
    /// Nudge an input payload at a matching event index in both traces.
    PayloadNudge {
        index: usize,
        field: String,
        direction: String,
    },
    /// Greedy minimizer removed paired events while preserving violation.
    Minimized { removed_events: usize },
}

/// Auditable witness for one solver/evaluator decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SolverWitness {
    /// Stable solver identifier.
    pub solver_id: String,
    /// Candidate identifier.
    pub candidate_id: String,
    /// Seed used by the deterministic candidate scheduler.
    pub seed: u64,
    /// Evaluation order.
    pub candidate_index: usize,
    /// Mutation strategy.
    pub mutation: TraceMutation,
    /// Machine-checkable constraints assumed by this candidate.
    pub constraints: Vec<String>,
    /// Concrete witness model values.
    pub model: BTreeMap<String, String>,
    /// Optimization objective used for candidate ranking/minimization.
    pub objective: String,
    /// Semantic verdict observed for this candidate.
    pub verdict: SemanticDiffVerdict,
}

/// Minimized divergence discovered by the explorer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcolicCounterexample {
    /// Stable counterexample id derived from minimized trace content.
    pub counterexample_id: String,
    /// Minimized source-side trace.
    pub source_trace: InteractionTrace,
    /// Minimized translated-side trace.
    pub translated_trace: InteractionTrace,
    /// First semantic counterexample reported by the comparator.
    pub semantic_counterexample: Option<CounterexampleTrace>,
    /// Semantic differences preserved by the minimized trace pair.
    pub differences: Vec<SemanticDifference>,
    /// Solver witness for the candidate that produced this counterexample.
    pub solver_witness: SolverWitness,
}

/// Exportable fixture bundle for regression tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionFixture {
    /// Fixture schema version.
    pub schema_version: String,
    /// Stable fixture id derived from the counterexample id.
    pub fixture_id: String,
    /// Minimized source-side trace.
    pub source_trace: InteractionTrace,
    /// Minimized translated-side trace.
    pub translated_trace: InteractionTrace,
    /// Expected semantic verdict when replayed through the differential oracle.
    pub expected_verdict: SemanticDiffVerdict,
    /// Solver witness metadata required to reproduce the fixture.
    pub solver_witness: SolverWitness,
    /// Replay command from the semantic comparator or trace metadata.
    pub replay_command: String,
}

/// Full exploration report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcolicExplorationReport {
    /// Report schema version.
    pub schema_version: String,
    /// Budget used for this run.
    pub budget: ConcolicBudget,
    /// Number of candidates evaluated.
    pub candidates_evaluated: usize,
    /// True when candidates were skipped because `max_candidates` was reached.
    pub budget_exhausted: bool,
    /// Per-candidate solver witnesses.
    pub solver_witnesses: Vec<SolverWitness>,
    /// Best minimized counterexample, if a violation was discovered.
    pub best_counterexample: Option<ConcolicCounterexample>,
    /// Regression fixture automatically exported from the best counterexample.
    pub regression_fixture: Option<RegressionFixture>,
}

/// Deterministic concolic differential explorer.
#[derive(Debug, Clone)]
pub struct ConcolicDifferentialExplorer {
    budget: ConcolicBudget,
}

impl ConcolicDifferentialExplorer {
    /// Create an explorer with the given budget.
    #[must_use]
    pub fn new(budget: ConcolicBudget) -> Self {
        Self { budget }
    }

    /// Create an explorer with default budget.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(ConcolicBudget::default())
    }

    /// Explore source/translated traces and return the best minimized violation.
    #[must_use]
    pub fn explore_traces(
        &self,
        source_trace: &InteractionTrace,
        translated_trace: &InteractionTrace,
    ) -> ConcolicExplorationReport {
        let mut candidates = self.generate_candidates(source_trace, translated_trace);
        let budget_exhausted = candidates.len() > self.budget.max_candidates;
        candidates.truncate(self.budget.max_candidates);

        let mut witnesses = Vec::with_capacity(candidates.len());
        let mut best: Option<(ConcolicCounterexample, usize)> = None;

        for (candidate_index, candidate) in candidates.into_iter().enumerate() {
            let diff = compare_traces(&candidate.source_trace, &candidate.translated_trace);
            let witness = solver_witness(&candidate, candidate_index, &self.budget, &diff);

            if diff.verdict == SemanticDiffVerdict::Violation {
                let minimized = self.minimize_candidate(candidate, witness.clone());
                let score = minimization_score(&minimized);
                if best
                    .as_ref()
                    .is_none_or(|(_, best_score)| score < *best_score)
                {
                    best = Some((minimized, score));
                }
            }

            witnesses.push(witness);
        }

        let best_counterexample = best.map(|(counterexample, _)| counterexample);
        let regression_fixture = best_counterexample.as_ref().map(export_regression_fixture);

        ConcolicExplorationReport {
            schema_version: CONCOLIC_DIFFERENTIAL_VERSION.to_string(),
            budget: self.budget.clone(),
            candidates_evaluated: witnesses.len(),
            budget_exhausted,
            solver_witnesses: witnesses,
            best_counterexample,
            regression_fixture,
        }
    }

    fn generate_candidates(
        &self,
        source_trace: &InteractionTrace,
        translated_trace: &InteractionTrace,
    ) -> Vec<TraceCandidate> {
        let max_events = source_trace
            .events
            .len()
            .min(translated_trace.events.len())
            .min(self.budget.max_events_per_candidate);
        let mut candidates = Vec::new();

        candidates.push(TraceCandidate::new(
            "candidate-baseline".to_string(),
            TraceMutation::Baseline,
            source_trace.clone(),
            translated_trace.clone(),
        ));

        for len in 1..=max_events {
            let source_prefix = prefix_events(&source_trace.events, len);
            let translated_prefix = prefix_events(&translated_trace.events, len);
            candidates.push(TraceCandidate::new(
                format!("candidate-prefix-{len}"),
                TraceMutation::Prefix { len },
                rebuild_trace(source_trace, &source_prefix, &format!("prefix-{len}")),
                rebuild_trace(
                    translated_trace,
                    &translated_prefix,
                    &format!("prefix-{len}"),
                ),
            ));
        }

        for index in ranked_indices(max_events, self.budget.seed) {
            let Some(source_singleton) = singleton_event(&source_trace.events, index) else {
                continue;
            };
            let Some(translated_singleton) = singleton_event(&translated_trace.events, index)
            else {
                continue;
            };
            candidates.push(TraceCandidate::new(
                format!("candidate-singleton-{index}"),
                TraceMutation::Singleton { index },
                rebuild_trace(
                    source_trace,
                    &source_singleton,
                    &format!("singleton-{index}"),
                ),
                rebuild_trace(
                    translated_trace,
                    &translated_singleton,
                    &format!("singleton-{index}"),
                ),
            ));
        }

        for index in ranked_indices(max_events, self.budget.seed ^ 0xA11E_1234)
            .into_iter()
            .take(self.budget.max_payload_mutations)
        {
            if let Some(candidate) = payload_nudge_candidate(source_trace, translated_trace, index)
            {
                candidates.push(candidate);
            }
        }

        candidates
    }

    fn minimize_candidate(
        &self,
        candidate: TraceCandidate,
        initial_witness: SolverWitness,
    ) -> ConcolicCounterexample {
        let mut source_events = candidate.source_trace.events.clone();
        let mut translated_events = candidate.translated_trace.events.clone();
        let initial_event_count = source_events.len() + translated_events.len();

        for _ in 0..self.budget.max_shrink_passes {
            let mut changed = false;
            let paired_len = source_events.len().min(translated_events.len());

            for index in 0..paired_len {
                if source_events.len() <= 1 || translated_events.len() <= 1 {
                    break;
                }

                let mut trial_source = source_events.clone();
                let mut trial_translated = translated_events.clone();
                trial_source.remove(index);
                trial_translated.remove(index);

                let trial_source_trace =
                    rebuild_trace(&candidate.source_trace, &trial_source, "minimize-source");
                let trial_translated_trace = rebuild_trace(
                    &candidate.translated_trace,
                    &trial_translated,
                    "minimize-translated",
                );
                let trial_report = compare_traces(&trial_source_trace, &trial_translated_trace);

                if trial_report.verdict == SemanticDiffVerdict::Violation {
                    source_events = trial_source;
                    translated_events = trial_translated;
                    changed = true;
                    break;
                }
            }

            if !changed {
                break;
            }
        }

        let source_trace = rebuild_trace(&candidate.source_trace, &source_events, "minimized");
        let translated_trace =
            rebuild_trace(&candidate.translated_trace, &translated_events, "minimized");
        let report = compare_traces(&source_trace, &translated_trace);
        let removed_events = initial_event_count
            .saturating_sub(source_trace.events.len() + translated_trace.events.len());
        let mut solver_witness = initial_witness;
        solver_witness.mutation = TraceMutation::Minimized { removed_events };
        solver_witness.model.insert(
            "minimized_source_events".to_string(),
            source_trace.events.len().to_string(),
        );
        solver_witness.model.insert(
            "minimized_translated_events".to_string(),
            translated_trace.events.len().to_string(),
        );

        let counterexample_id = counterexample_id(&source_trace, &translated_trace);

        ConcolicCounterexample {
            counterexample_id,
            source_trace,
            translated_trace,
            semantic_counterexample: report.counterexample,
            differences: report.differences,
            solver_witness,
        }
    }
}

#[derive(Debug, Clone)]
struct TraceCandidate {
    candidate_id: String,
    mutation: TraceMutation,
    source_trace: InteractionTrace,
    translated_trace: InteractionTrace,
}

impl TraceCandidate {
    fn new(
        candidate_id: String,
        mutation: TraceMutation,
        source_trace: InteractionTrace,
        translated_trace: InteractionTrace,
    ) -> Self {
        Self {
            candidate_id,
            mutation,
            source_trace,
            translated_trace,
        }
    }
}

fn solver_witness(
    candidate: &TraceCandidate,
    candidate_index: usize,
    budget: &ConcolicBudget,
    diff: &SemanticDiffReport,
) -> SolverWitness {
    let mut model = BTreeMap::new();
    model.insert(
        "source_trace_id".to_string(),
        candidate.source_trace.trace_id.clone(),
    );
    model.insert(
        "translated_trace_id".to_string(),
        candidate.translated_trace.trace_id.clone(),
    );
    model.insert(
        "source_events".to_string(),
        candidate.source_trace.events.len().to_string(),
    );
    model.insert(
        "translated_events".to_string(),
        candidate.translated_trace.events.len().to_string(),
    );
    model.insert(
        "differences".to_string(),
        diff.differences.len().to_string(),
    );

    SolverWitness {
        solver_id: CONCOLIC_DIFFERENTIAL_VERSION.to_string(),
        candidate_id: candidate.candidate_id.clone(),
        seed: budget.seed,
        candidate_index,
        mutation: candidate.mutation.clone(),
        constraints: vec![
            "preserve event order within each candidate trace".to_string(),
            "compare only canonical semantic observations".to_string(),
            "rank counterexamples by minimized total event count".to_string(),
        ],
        model,
        objective: "minimize source_events + translated_events while preserving violation"
            .to_string(),
        verdict: diff.verdict,
    }
}

fn export_regression_fixture(counterexample: &ConcolicCounterexample) -> RegressionFixture {
    let fixture_id = format!("concolic-fixture:{}", counterexample.counterexample_id);
    let replay_command = counterexample
        .semantic_counterexample
        .as_ref()
        .map_or_else(String::new, |trace| trace.replay_command.clone());

    RegressionFixture {
        schema_version: CONCOLIC_DIFFERENTIAL_VERSION.to_string(),
        fixture_id,
        source_trace: counterexample.source_trace.clone(),
        translated_trace: counterexample.translated_trace.clone(),
        expected_verdict: SemanticDiffVerdict::Violation,
        solver_witness: counterexample.solver_witness.clone(),
        replay_command,
    }
}

fn payload_nudge_candidate(
    source_trace: &InteractionTrace,
    translated_trace: &InteractionTrace,
    index: usize,
) -> Option<TraceCandidate> {
    let source_event = source_trace.events.get(index)?;
    let translated_event = translated_trace.events.get(index)?;
    let (source_nudged, field, direction) = nudge_input_event(source_event)?;
    let (translated_nudged, _, _) = nudge_input_event(translated_event)?;

    let mut source_events = source_trace.events.clone();
    let mut translated_events = translated_trace.events.clone();
    let source_slot = source_events.get_mut(index)?;
    *source_slot = source_nudged;
    let translated_slot = translated_events.get_mut(index)?;
    *translated_slot = translated_nudged;

    Some(TraceCandidate::new(
        format!("candidate-payload-nudge-{index}-{field}-{direction}"),
        TraceMutation::PayloadNudge {
            index,
            field,
            direction,
        },
        rebuild_trace(
            source_trace,
            &source_events,
            &format!("payload-nudge-{index}"),
        ),
        rebuild_trace(
            translated_trace,
            &translated_events,
            &format!("payload-nudge-{index}"),
        ),
    ))
}

fn nudge_input_event(event: &TraceEvent) -> Option<(TraceEvent, String, String)> {
    let mut nudged = event.clone();
    let (field, direction) = match &mut nudged.payload {
        TracePayload::Key {
            key,
            modifiers,
            action,
        } => {
            if modifiers
                .iter()
                .any(|modifier| modifier == "concolic_shift")
            {
                key.push_str("_concolic");
                ("key".to_string(), "suffix".to_string())
            } else {
                modifiers.push("concolic_shift".to_string());
                *action = match action {
                    KeyAction::Press => KeyAction::Repeat,
                    KeyAction::Release | KeyAction::Repeat => KeyAction::Press,
                };
                ("modifiers".to_string(), "add_shift".to_string())
            }
        }
        TracePayload::TextInput { text } => {
            text.push('x');
            ("text".to_string(), "append_x".to_string())
        }
        TracePayload::Mouse { x, y, action, .. } => {
            *x = x.saturating_add(1);
            *y = y.saturating_add(1);
            *action = match action {
                MouseAction::Press | MouseAction::Move => MouseAction::Drag,
                MouseAction::Release | MouseAction::Drag => MouseAction::Move,
            };
            (
                "mouse_position".to_string(),
                "diagonal_plus_one".to_string(),
            )
        }
        TracePayload::Scroll {
            delta_x, delta_y, ..
        } => {
            *delta_x = delta_x.saturating_add(1);
            *delta_y = delta_y.saturating_sub(1);
            ("scroll_delta".to_string(), "x_plus_y_minus".to_string())
        }
        TracePayload::Resize { width, height } => {
            *width = width.saturating_add(1);
            *height = height.saturating_add(1);
            ("viewport".to_string(), "grow_one".to_string())
        }
        TracePayload::RenderCapture { .. }
        | TracePayload::StateCapture { .. }
        | TracePayload::Marker { .. } => return None,
    };

    Some((nudged, field, direction))
}

fn rebuild_trace(base: &InteractionTrace, events: &[TraceEvent], suffix: &str) -> InteractionTrace {
    let mut builder = TraceBuilder::new(
        format!("{}::{suffix}", base.trace_id),
        base.run_id.clone(),
        base.initial_viewport,
    );
    for (key, value) in &base.metadata {
        builder = builder.with_metadata(key.clone(), value.clone());
    }
    builder = builder.with_metadata("concolic_parent_trace", base.trace_id.clone());
    builder = builder.with_metadata("concolic_suffix", suffix.to_string());

    for event in events {
        builder.record_labeled(event.offset_ms, event.payload.clone(), event.label.clone());
    }

    builder.build()
}

fn ranked_indices(len: usize, seed: u64) -> Vec<usize> {
    let mut indices = (0..len).collect::<Vec<_>>();
    indices.sort_by_key(|index| {
        let index_key = u64::try_from(*index).unwrap_or(u64::MAX);
        deterministic_mix(seed ^ index_key)
    });
    indices
}

fn prefix_events(events: &[TraceEvent], len: usize) -> Vec<TraceEvent> {
    events.iter().take(len).cloned().collect()
}

fn singleton_event(events: &[TraceEvent], index: usize) -> Option<Vec<TraceEvent>> {
    events.get(index).cloned().map(|event| vec![event])
}

fn deterministic_mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn minimization_score(counterexample: &ConcolicCounterexample) -> usize {
    counterexample.source_trace.events.len() + counterexample.translated_trace.events.len()
}

fn counterexample_id(
    source_trace: &InteractionTrace,
    translated_trace: &InteractionTrace,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CONCOLIC_DIFFERENTIAL_VERSION.as_bytes());
    hasher.update(source_trace.events_hash.as_bytes());
    hasher.update(translated_trace.events_hash.as_bytes());
    format!("concolic-cex:{}", hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{KeyAction, MouseButton, TracePayload, Viewport};

    fn viewport() -> Viewport {
        Viewport {
            width: 80,
            height: 24,
        }
    }

    fn divergent_trace_pair() -> (InteractionTrace, InteractionTrace) {
        let mut source = TraceBuilder::new("source", "source-run", viewport())
            .with_metadata("replay_command", "doctor_frankentui replay source");
        source.record(
            0,
            TracePayload::Marker {
                name: "start".to_string(),
            },
        );
        source.record(
            5,
            TracePayload::Key {
                key: "a".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
        source.record(
            10,
            TracePayload::StateCapture {
                state_hash: "state-a".to_string(),
                component: Some("counter".to_string()),
            },
        );

        let mut translated = TraceBuilder::new("translated", "translated-run", viewport())
            .with_metadata("replay_command", "doctor_frankentui replay translated");
        translated.record(
            0,
            TracePayload::Marker {
                name: "start".to_string(),
            },
        );
        translated.record(
            5,
            TracePayload::Key {
                key: "b".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
        translated.record(
            10,
            TracePayload::StateCapture {
                state_hash: "state-b".to_string(),
                component: Some("counter".to_string()),
            },
        );

        (source.build(), translated.build())
    }

    fn equivalent_trace_pair() -> (InteractionTrace, InteractionTrace) {
        let mut source = TraceBuilder::new("source-eq", "source-run", viewport());
        source.record(
            0,
            TracePayload::Key {
                key: "a".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
        source.record(
            5,
            TracePayload::Resize {
                width: 100,
                height: 40,
            },
        );

        let mut translated = TraceBuilder::new("translated-eq", "translated-run", viewport());
        translated.record(
            0,
            TracePayload::Key {
                key: "a".to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
        translated.record(
            5,
            TracePayload::Resize {
                width: 100,
                height: 40,
            },
        );

        (source.build(), translated.build())
    }

    #[test]
    fn discovers_minimized_counterexample_with_solver_metadata() {
        let (source, translated) = divergent_trace_pair();
        let explorer = ConcolicDifferentialExplorer::new(ConcolicBudget {
            max_candidates: 32,
            ..ConcolicBudget::default()
        });

        let report = explorer.explore_traces(&source, &translated);
        assert!(
            report.best_counterexample.is_some(),
            "divergent traces should yield a counterexample"
        );
        let Some(counterexample) = report.best_counterexample.as_ref() else {
            return;
        };

        assert_eq!(report.schema_version, CONCOLIC_DIFFERENTIAL_VERSION);
        assert!(report.candidates_evaluated <= 32);
        assert_eq!(
            counterexample.solver_witness.solver_id,
            CONCOLIC_DIFFERENTIAL_VERSION
        );
        assert_eq!(counterexample.solver_witness.seed, 0xC0DE_C0DE);
        assert!(!counterexample.differences.is_empty());
        assert!(
            counterexample.source_trace.events.len() < source.events.len()
                || counterexample.translated_trace.events.len() < translated.events.len(),
            "counterexample should be minimized below the original trace pair"
        );
        assert!(report.regression_fixture.is_some());
    }

    #[test]
    fn zero_candidate_budget_fails_closed_without_silent_pass() {
        let (source, translated) = divergent_trace_pair();
        let explorer = ConcolicDifferentialExplorer::new(ConcolicBudget {
            max_candidates: 0,
            ..ConcolicBudget::default()
        });

        let report = explorer.explore_traces(&source, &translated);

        assert!(report.budget_exhausted);
        assert_eq!(report.candidates_evaluated, 0);
        assert!(report.best_counterexample.is_none());
        assert!(report.regression_fixture.is_none());
    }

    #[test]
    fn deterministic_seed_controls_payload_mutation_order() {
        let (source, translated) = equivalent_trace_pair();
        let budget = ConcolicBudget {
            seed: 7,
            max_candidates: 16,
            max_payload_mutations: 8,
            ..ConcolicBudget::default()
        };
        let explorer = ConcolicDifferentialExplorer::new(budget);

        let first = explorer.explore_traces(&source, &translated);
        let second = explorer.explore_traces(&source, &translated);

        assert_eq!(first.solver_witnesses, second.solver_witnesses);
        assert!(first.solver_witnesses.iter().any(|witness| {
            matches!(
                witness.mutation,
                TraceMutation::PayloadNudge {
                    index: _,
                    field: _,
                    direction: _
                }
            )
        }));
    }

    #[test]
    fn regression_fixture_export_is_replayable_and_stable() {
        let (source, translated) = divergent_trace_pair();
        let explorer = ConcolicDifferentialExplorer::with_defaults();

        let first = explorer.explore_traces(&source, &translated);
        let second = explorer.explore_traces(&source, &translated);
        assert!(
            first.regression_fixture.is_some(),
            "fixture should be exported"
        );
        assert!(
            second.regression_fixture.is_some(),
            "fixture should be exported"
        );
        let Some(first_fixture) = first.regression_fixture.as_ref() else {
            return;
        };
        let Some(second_fixture) = second.regression_fixture.as_ref() else {
            return;
        };

        assert_eq!(first_fixture.fixture_id, second_fixture.fixture_id);
        assert_eq!(
            first_fixture.source_trace.events_hash,
            second_fixture.source_trace.events_hash
        );
        assert_eq!(
            first_fixture.translated_trace.events_hash,
            second_fixture.translated_trace.events_hash
        );
        assert_eq!(
            first_fixture.expected_verdict,
            SemanticDiffVerdict::Violation
        );
        assert!(first_fixture.replay_command.contains("doctor_frankentui"));
    }

    #[test]
    fn payload_nudge_preserves_trace_integrity_metadata() {
        let mut builder = TraceBuilder::new("source-resize", "run", viewport());
        builder.record(
            0,
            TracePayload::Resize {
                width: 80,
                height: 24,
            },
        );
        let source = builder.build();
        let translated = source.clone();

        let candidate = payload_nudge_candidate(&source, &translated, 0);
        assert!(candidate.is_some(), "resize can be nudged");
        let Some(candidate) = candidate else {
            return;
        };

        assert_eq!(
            candidate
                .source_trace
                .metadata
                .get("concolic_parent_trace")
                .map(String::as_str),
            Some("source-resize")
        );
        assert_ne!(candidate.source_trace.events_hash, source.events_hash);
    }

    #[test]
    fn input_payload_nudges_cover_terminal_event_shapes() {
        let events = [
            TraceEvent {
                offset_ms: 0,
                sequence: 0,
                payload: TracePayload::Mouse {
                    x: 1,
                    y: 2,
                    button: MouseButton::Left,
                    action: MouseAction::Move,
                },
                label: None,
            },
            TraceEvent {
                offset_ms: 0,
                sequence: 0,
                payload: TracePayload::Scroll {
                    x: 1,
                    y: 2,
                    delta_x: 0,
                    delta_y: 1,
                },
                label: None,
            },
            TraceEvent {
                offset_ms: 0,
                sequence: 0,
                payload: TracePayload::TextInput {
                    text: "paste".to_string(),
                },
                label: None,
            },
        ];

        for event in events {
            let nudged = nudge_input_event(&event);
            assert!(nudged.is_some(), "input event should be mutable");
        }
    }
}
