//! Semantic differential comparison for source and translated migration runs.
//!
//! The comparator works over canonical semantic observations extracted from
//! interaction traces or assembled directly by certification harnesses. It keeps
//! verdicts tied to semantic contract clause IDs and feeds observed
//! pass/failure counts through the existing confidence model.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::semantic_contract::{
    ExpectedLossResult, SemanticEquivalenceContract, TransformationRiskLevel,
    load_builtin_confidence_model, load_builtin_semantic_contract,
};
use crate::trace::{InteractionTrace, MouseAction, MouseButton, TracePayload};

pub const SEMANTIC_DIFF_VALIDATOR_ID: &str = "semantic_diff_validator";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDiffVerdict {
    Equivalent,
    AcceptableImprovement,
    Violation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SemanticObservationKind {
    EventOrdering,
    StateTransition,
    SideEffect,
    Improvement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticObservation {
    pub sequence: u32,
    pub offset_ms: u64,
    pub kind: SemanticObservationKind,
    pub key: String,
    pub value: String,
    pub contract_clause_ids: Vec<String>,
}

impl SemanticObservation {
    #[must_use]
    pub fn new(
        sequence: u32,
        offset_ms: u64,
        kind: SemanticObservationKind,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            sequence,
            offset_ms,
            kind,
            key: key.into(),
            value: value.into(),
            contract_clause_ids: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_contract_clause_ids(mut self, clause_ids: Vec<String>) -> Self {
        self.contract_clause_ids = clause_ids;
        self
    }

    #[must_use]
    pub fn comparable_signature(&self) -> String {
        format!("{:?}:{}={}", self.kind, self.key, self.value)
    }

    #[must_use]
    pub fn core_eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.key == other.key && self.value == other.value
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticRun {
    pub run_id: String,
    pub trace_id: Option<String>,
    pub replay_command: Option<String>,
    pub observations: Vec<SemanticObservation>,
}

impl SemanticRun {
    #[must_use]
    pub fn new(run_id: impl Into<String>, observations: Vec<SemanticObservation>) -> Self {
        Self {
            run_id: run_id.into(),
            trace_id: None,
            replay_command: None,
            observations: canonicalize_observation_order(observations),
        }
    }

    #[must_use]
    pub fn from_trace(trace: &InteractionTrace) -> Self {
        let replay_command = trace.metadata.get("replay_command").cloned();
        Self {
            run_id: trace.run_id.clone(),
            trace_id: Some(trace.trace_id.clone()),
            replay_command,
            observations: observations_from_trace(trace),
        }
    }

    #[must_use]
    pub fn with_replay_command(mut self, replay_command: impl Into<String>) -> Self {
        self.replay_command = Some(replay_command.into());
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDifferenceKind {
    ValueMismatch,
    MissingObservation,
    UnexpectedObservation,
    ForbiddenImprovement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticDifference {
    pub difference_kind: SemanticDifferenceKind,
    pub observation_kind: SemanticObservationKind,
    pub key: String,
    pub source_value: Option<String>,
    pub translated_value: Option<String>,
    pub clause_ids: Vec<String>,
    pub risk_level: TransformationRiskLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CounterexampleTrace {
    pub divergence_index: usize,
    pub source_observations: Vec<SemanticObservation>,
    pub translated_observations: Vec<SemanticObservation>,
    pub replay_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticDiffReport {
    pub validator_id: String,
    pub contract_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub verdict: SemanticDiffVerdict,
    pub observations_compared: usize,
    pub differences: Vec<SemanticDifference>,
    pub counterexample: Option<CounterexampleTrace>,
    pub covered_clause_ids: Vec<String>,
    pub violated_clause_ids: Vec<String>,
    pub risk_level: TransformationRiskLevel,
    pub risk_score: f64,
    pub expected_loss: ExpectedLossResult,
}

#[must_use]
pub fn compare_traces(
    source_trace: &InteractionTrace,
    translated_trace: &InteractionTrace,
) -> SemanticDiffReport {
    let source_run = SemanticRun::from_trace(source_trace);
    let translated_run = SemanticRun::from_trace(translated_trace);
    compare_runs(&source_run, &translated_run)
}

#[must_use]
pub fn compare_runs(source_run: &SemanticRun, translated_run: &SemanticRun) -> SemanticDiffReport {
    let contract = load_builtin_semantic_contract().expect("built-in semantic contract must parse");
    compare_runs_with_contract(source_run, translated_run, &contract)
}

#[must_use]
pub fn compare_runs_with_contract(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    contract: &SemanticEquivalenceContract,
) -> SemanticDiffReport {
    let source_observations = normalize_observations(&source_run.observations);
    let translated_observations = normalize_observations(&translated_run.observations);
    let source_core = source_observations
        .iter()
        .filter(|obs| obs.kind != SemanticObservationKind::Improvement)
        .cloned()
        .collect::<Vec<_>>();
    let translated_core = translated_observations
        .iter()
        .filter(|obs| obs.kind != SemanticObservationKind::Improvement)
        .cloned()
        .collect::<Vec<_>>();

    let clause_risks = clause_risk_map(contract);
    let mut differences = Vec::new();
    let mut covered_clause_ids = BTreeSet::new();
    let mut violated_clause_ids = BTreeSet::new();
    let mut successes = 0_u32;
    let mut weighted_failures = 0_u32;
    let max_len = source_core.len().max(translated_core.len());

    for index in 0..max_len {
        match (source_core.get(index), translated_core.get(index)) {
            (Some(source), Some(translated)) if source.core_eq(translated) => {
                successes = successes.saturating_add(1);
                covered_clause_ids.extend(source.contract_clause_ids.iter().cloned());
            }
            (Some(source), Some(translated)) => {
                let clauses = merged_clause_ids(source, translated);
                let risk_level = risk_for_clauses(&clauses, &clause_risks);
                weighted_failures = weighted_failures.saturating_add(failure_weight(risk_level));
                violated_clause_ids.extend(clauses.iter().cloned());
                differences.push(SemanticDifference {
                    difference_kind: SemanticDifferenceKind::ValueMismatch,
                    observation_kind: source.kind,
                    key: source.key.clone(),
                    source_value: Some(source.comparable_signature()),
                    translated_value: Some(translated.comparable_signature()),
                    clause_ids: clauses,
                    risk_level,
                    message: format!(
                        "semantic observation diverged at index {index}: source '{}' translated '{}'",
                        source.comparable_signature(),
                        translated.comparable_signature()
                    ),
                });
            }
            (Some(source), None) => {
                let clauses = source.contract_clause_ids.clone();
                let risk_level = risk_for_clauses(&clauses, &clause_risks);
                weighted_failures = weighted_failures.saturating_add(failure_weight(risk_level));
                violated_clause_ids.extend(clauses.iter().cloned());
                differences.push(SemanticDifference {
                    difference_kind: SemanticDifferenceKind::MissingObservation,
                    observation_kind: source.kind,
                    key: source.key.clone(),
                    source_value: Some(source.comparable_signature()),
                    translated_value: None,
                    clause_ids: clauses,
                    risk_level,
                    message: format!(
                        "translated run dropped source semantic observation '{}'",
                        source.comparable_signature()
                    ),
                });
            }
            (None, Some(translated)) => {
                let clauses = translated.contract_clause_ids.clone();
                let risk_level = risk_for_clauses(&clauses, &clause_risks);
                weighted_failures = weighted_failures.saturating_add(failure_weight(risk_level));
                violated_clause_ids.extend(clauses.iter().cloned());
                differences.push(SemanticDifference {
                    difference_kind: SemanticDifferenceKind::UnexpectedObservation,
                    observation_kind: translated.kind,
                    key: translated.key.clone(),
                    source_value: None,
                    translated_value: Some(translated.comparable_signature()),
                    clause_ids: clauses,
                    risk_level,
                    message: format!(
                        "translated run introduced non-improvement semantic observation '{}'",
                        translated.comparable_signature()
                    ),
                });
            }
            (None, None) => {}
        }
    }

    let allowed_improvements = translated_observations
        .iter()
        .filter(|obs| obs.kind == SemanticObservationKind::Improvement)
        .filter(|obs| is_allowed_improvement(obs, contract))
        .count();
    let allowed_improvements = usize_to_u32_saturating(allowed_improvements);
    successes = successes.saturating_add(allowed_improvements);
    covered_clause_ids.extend(
        translated_observations
            .iter()
            .filter(|obs| obs.kind == SemanticObservationKind::Improvement)
            .filter(|obs| is_allowed_improvement(obs, contract))
            .flat_map(|obs| obs.contract_clause_ids.iter().cloned()),
    );

    for improvement in translated_observations
        .iter()
        .filter(|obs| obs.kind == SemanticObservationKind::Improvement)
        .filter(|obs| !is_allowed_improvement(obs, contract))
    {
        let clauses = vec!["IE-002".to_string()];
        let risk_level = risk_for_clauses(&clauses, &clause_risks);
        weighted_failures = weighted_failures.saturating_add(failure_weight(risk_level));
        violated_clause_ids.extend(clauses.iter().cloned());
        differences.push(SemanticDifference {
            difference_kind: SemanticDifferenceKind::ForbiddenImprovement,
            observation_kind: improvement.kind,
            key: improvement.key.clone(),
            source_value: None,
            translated_value: Some(improvement.comparable_signature()),
            clause_ids: clauses,
            risk_level,
            message: format!(
                "translated run claims forbidden or undeclared improvement '{}'",
                improvement.key
            ),
        });
    }

    let verdict = if differences.is_empty() {
        if allowed_improvements > 0 {
            SemanticDiffVerdict::AcceptableImprovement
        } else {
            SemanticDiffVerdict::Equivalent
        }
    } else {
        SemanticDiffVerdict::Violation
    };
    let risk_level = differences
        .iter()
        .map(|diff| diff.risk_level)
        .max()
        .unwrap_or(TransformationRiskLevel::Low);
    let risk_score = risk_score(successes, weighted_failures);
    let first_violated_clause = violated_clause_ids.iter().next().cloned();
    let expected_loss = expected_loss(successes, weighted_failures, first_violated_clause);
    let counterexample = first_counterexample(
        source_run,
        translated_run,
        &source_core,
        &translated_core,
        &translated_observations,
        &differences,
    );

    SemanticDiffReport {
        validator_id: SEMANTIC_DIFF_VALIDATOR_ID.to_string(),
        contract_id: contract.contract_id.clone(),
        source_run_id: source_run.run_id.clone(),
        translated_run_id: translated_run.run_id.clone(),
        verdict,
        observations_compared: max_len,
        differences,
        counterexample,
        covered_clause_ids: covered_clause_ids.into_iter().collect(),
        violated_clause_ids: violated_clause_ids.into_iter().collect(),
        risk_level,
        risk_score,
        expected_loss,
    }
}

#[must_use]
pub fn observations_from_trace(trace: &InteractionTrace) -> Vec<SemanticObservation> {
    let observations = trace
        .events
        .iter()
        .filter_map(|event| {
            let mut observation = match &event.payload {
                TracePayload::Key {
                    key,
                    modifiers,
                    action,
                } => SemanticObservation::new(
                    event.sequence,
                    event.offset_ms,
                    SemanticObservationKind::EventOrdering,
                    format!("key:{key}"),
                    format!("modifiers={};action={action:?}", modifiers.join("+")),
                ),
                TracePayload::TextInput { text } => SemanticObservation::new(
                    event.sequence,
                    event.offset_ms,
                    SemanticObservationKind::EventOrdering,
                    "text_input",
                    text.clone(),
                ),
                TracePayload::Mouse {
                    x,
                    y,
                    button,
                    action,
                } => SemanticObservation::new(
                    event.sequence,
                    event.offset_ms,
                    SemanticObservationKind::EventOrdering,
                    format!("mouse:{}", mouse_button_label(*button)),
                    format!("x={x};y={y};action={}", mouse_action_label(*action)),
                ),
                TracePayload::Scroll {
                    x,
                    y,
                    delta_x,
                    delta_y,
                } => SemanticObservation::new(
                    event.sequence,
                    event.offset_ms,
                    SemanticObservationKind::EventOrdering,
                    "scroll",
                    format!("x={x};y={y};dx={delta_x};dy={delta_y}"),
                ),
                TracePayload::Resize { width, height } => SemanticObservation::new(
                    event.sequence,
                    event.offset_ms,
                    SemanticObservationKind::EventOrdering,
                    "resize",
                    format!("width={width};height={height}"),
                ),
                TracePayload::StateCapture {
                    state_hash,
                    component,
                } => SemanticObservation::new(
                    event.sequence,
                    event.offset_ms,
                    SemanticObservationKind::StateTransition,
                    component.as_deref().unwrap_or("global"),
                    state_hash.clone(),
                ),
                TracePayload::Marker { name } => marker_observation(
                    event.sequence,
                    event.offset_ms,
                    name,
                    event.label.as_deref(),
                )?,
                TracePayload::RenderCapture { .. } => return None,
            };
            observation.contract_clause_ids = default_clause_ids(observation.kind);
            Some(observation)
        })
        .collect::<Vec<_>>();
    canonicalize_observation_order(observations)
}

fn marker_observation(
    sequence: u32,
    offset_ms: u64,
    name: &str,
    label: Option<&str>,
) -> Option<SemanticObservation> {
    let (kind, rest) = if let Some(rest) = name.strip_prefix("effect:") {
        (SemanticObservationKind::SideEffect, rest)
    } else if let Some(rest) = name.strip_prefix("event:") {
        (SemanticObservationKind::EventOrdering, rest)
    } else {
        let rest = name.strip_prefix("improvement:")?;
        (SemanticObservationKind::Improvement, rest)
    };
    let (key, value) = split_key_value(rest, label.unwrap_or("observed"));
    Some(SemanticObservation::new(
        sequence, offset_ms, kind, key, value,
    ))
}

fn split_key_value(raw: &str, fallback: &str) -> (String, String) {
    raw.split_once('=').map_or_else(
        || (raw.to_string(), fallback.to_string()),
        |(key, value)| (key.to_string(), value.to_string()),
    )
}

fn normalize_observations(observations: &[SemanticObservation]) -> Vec<SemanticObservation> {
    canonicalize_observation_order(
        observations
            .iter()
            .cloned()
            .map(|mut obs| {
                if obs.contract_clause_ids.is_empty() {
                    obs.contract_clause_ids = default_clause_ids(obs.kind);
                } else {
                    obs.contract_clause_ids = dedupe_sorted(obs.contract_clause_ids);
                }
                obs
            })
            .collect(),
    )
}

fn canonicalize_observation_order(
    mut observations: Vec<SemanticObservation>,
) -> Vec<SemanticObservation> {
    observations.sort_by(|a, b| {
        a.sequence
            .cmp(&b.sequence)
            .then_with(|| a.offset_ms.cmp(&b.offset_ms))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.value.cmp(&b.value))
    });
    observations
}

fn default_clause_ids(kind: SemanticObservationKind) -> Vec<String> {
    match kind {
        SemanticObservationKind::EventOrdering => vec!["EO-001".to_string()],
        SemanticObservationKind::StateTransition => {
            vec!["ST-001".to_string(), "ST-002".to_string()]
        }
        SemanticObservationKind::SideEffect => vec!["SE-001".to_string()],
        SemanticObservationKind::Improvement => vec!["IE-001".to_string()],
    }
}

fn merged_clause_ids(
    source: &SemanticObservation,
    translated: &SemanticObservation,
) -> Vec<String> {
    dedupe_sorted(
        source
            .contract_clause_ids
            .iter()
            .chain(translated.contract_clause_ids.iter())
            .cloned()
            .collect(),
    )
}

fn dedupe_sorted(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn clause_risk_map(
    contract: &SemanticEquivalenceContract,
) -> BTreeMap<String, TransformationRiskLevel> {
    contract
        .clauses
        .iter()
        .map(|clause| (clause.clause_id.clone(), severity_to_risk(&clause.severity)))
        .collect()
}

fn risk_for_clauses(
    clause_ids: &[String],
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> TransformationRiskLevel {
    clause_ids
        .iter()
        .map(|clause_id| {
            clause_risks
                .get(clause_id)
                .copied()
                .unwrap_or(TransformationRiskLevel::Critical)
        })
        .max()
        .unwrap_or(TransformationRiskLevel::Medium)
}

fn severity_to_risk(severity: &str) -> TransformationRiskLevel {
    match severity {
        "critical" => TransformationRiskLevel::Critical,
        "high" => TransformationRiskLevel::High,
        "medium" => TransformationRiskLevel::Medium,
        _ => TransformationRiskLevel::Low,
    }
}

fn failure_weight(risk: TransformationRiskLevel) -> u32 {
    match risk {
        TransformationRiskLevel::Low => 1,
        TransformationRiskLevel::Medium => 2,
        TransformationRiskLevel::High => 4,
        TransformationRiskLevel::Critical => 8,
    }
}

fn risk_score(successes: u32, weighted_failures: u32) -> f64 {
    if weighted_failures == 0 {
        return 0.0;
    }
    let total = successes.saturating_add(weighted_failures);
    f64::from(weighted_failures) / f64::from(total)
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn expected_loss(
    successes: u32,
    weighted_failures: u32,
    claim_id: Option<String>,
) -> ExpectedLossResult {
    let confidence_model =
        load_builtin_confidence_model().expect("built-in confidence model must parse");
    let posterior = confidence_model.compute_posterior(successes, weighted_failures);
    confidence_model.expected_loss_decision(
        &posterior,
        claim_id,
        Some(SEMANTIC_DIFF_VALIDATOR_ID.to_string()),
    )
}

fn is_allowed_improvement(
    observation: &SemanticObservation,
    contract: &SemanticEquivalenceContract,
) -> bool {
    contract
        .improvement_envelope
        .allowed_dimensions
        .iter()
        .any(|dimension| dimension == &observation.key)
}

fn first_counterexample(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    source_core: &[SemanticObservation],
    translated_core: &[SemanticObservation],
    translated_observations: &[SemanticObservation],
    differences: &[SemanticDifference],
) -> Option<CounterexampleTrace> {
    let first_difference = differences.first()?;
    let divergence_index = divergence_index(source_core, translated_core, first_difference);
    let source_observations = source_core
        .get(divergence_index)
        .cloned()
        .into_iter()
        .collect::<Vec<_>>();
    let mut translated_window = translated_core
        .get(divergence_index)
        .cloned()
        .into_iter()
        .collect::<Vec<_>>();

    if translated_window.is_empty() {
        translated_window.extend(
            translated_observations
                .iter()
                .filter(|obs| obs.key == first_difference.key)
                .cloned(),
        );
    }

    Some(CounterexampleTrace {
        divergence_index,
        source_observations,
        translated_observations: translated_window,
        replay_command: replay_command(source_run, translated_run),
    })
}

fn divergence_index(
    source_core: &[SemanticObservation],
    translated_core: &[SemanticObservation],
    first_difference: &SemanticDifference,
) -> usize {
    source_core
        .iter()
        .zip(translated_core.iter())
        .position(|(source, translated)| !source.core_eq(translated))
        .or_else(|| {
            source_core
                .iter()
                .position(|obs| obs.key == first_difference.key)
        })
        .or_else(|| {
            translated_core
                .iter()
                .position(|obs| obs.key == first_difference.key)
        })
        .unwrap_or_else(|| source_core.len().min(translated_core.len()))
}

fn replay_command(source_run: &SemanticRun, translated_run: &SemanticRun) -> String {
    match (&source_run.replay_command, &translated_run.replay_command) {
        (Some(source), Some(translated)) => format!("{source} && {translated}"),
        (Some(source), None) => source.clone(),
        (None, Some(translated)) => translated.clone(),
        (None, None) => match (&source_run.trace_id, &translated_run.trace_id) {
            (Some(source_trace), Some(translated_trace)) => format!(
                "doctor_frankentui replay --trace-id {source_trace} && doctor_frankentui replay --trace-id {translated_trace}"
            ),
            _ => format!(
                "doctor_frankentui semantic-diff --source-run {} --translated-run {}",
                source_run.run_id, translated_run.run_id
            ),
        },
    }
}

fn mouse_button_label(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
        MouseButton::None => "none",
    }
}

fn mouse_action_label(action: MouseAction) -> &'static str {
    match action {
        MouseAction::Press => "press",
        MouseAction::Release => "release",
        MouseAction::Move => "move",
        MouseAction::Drag => "drag",
    }
}
