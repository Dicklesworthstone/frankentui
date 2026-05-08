// SPDX-License-Identifier: Apache-2.0
//! Abstract interpretation safety layer with Galois-connected over-approximation.
//!
//! Proves safety properties over the canonical effect model and migration IR
//! by computing fixpoints in abstract domains. Every property is classified as
//! `Proven`, `Refuted` (with counterexample), or `Unknown` (conservative
//! fallback — never silent pass).
//!
//! # Galois connection
//!
//! Each abstract domain `D` comes with:
//! - **α** (abstraction): concrete set → abstract element
//! - **γ** (concretization): abstract element → over-approximated concrete set
//!
//! Soundness contract: for all concrete sets `S`, `S ⊆ γ(α(S))`.
//! This ensures any property proven in the abstract domain holds concretely.
//!
//! # Safety properties
//!
//! - `EffectOrderingSafety`: ordering constraints form a DAG (no cycles)
//! - `NoForbiddenSideEffects`: no effect writes to a forbidden state set
//! - `DeterminismGuarantee`: all effects on replay-critical paths are deterministic
//! - `CleanupCompleteness`: every subscription has a cleanup path
//! - `IdempotencePreservation`: idempotent effects remain idempotent after transform
//!
//! # Migration rationale
//!
//! Source frameworks often have implicit ordering guarantees (React useEffect
//! order, Vue watch order) that must be preserved. Abstract interpretation
//! lets us verify these properties hold in the generated FrankenTUI code
//! without executing it.

#![forbid(unsafe_code)]

use crate::effect_canonical::{
    CanonicalEffect, CanonicalEffectModel, CleanupStrategy, ExecutionModel,
};
use crate::migration_ir::IrNodeId;
use std::collections::{BTreeMap, BTreeSet};

/// Version tag for the abstract interpretation domains and Galois contracts.
///
/// Increment this when any domain, abstraction map, concretization map, or
/// conservative fallback rule changes in a way that affects proof meaning.
pub const ABSTRACT_INTERPRETATION_DOMAIN_VERSION: &str = "abstract-interpretation-domains-v1";

// ─── Verification status ────────────────────────────────────────────────────

/// Outcome of verifying a safety property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationStatus {
    /// Property proven to hold in all reachable states.
    Proven,
    /// Property refuted with a concrete counterexample.
    Refuted {
        /// Human-readable description of the violation.
        reason: String,
    },
    /// Analysis could not determine status.
    /// **Conservative fallback: treat as unsafe.**
    Unknown {
        /// Why the analysis was inconclusive.
        reason: String,
    },
}

impl VerificationStatus {
    /// Returns `true` only if the property was definitively proven.
    pub fn is_safe(&self) -> bool {
        matches!(self, Self::Proven)
    }

    /// Returns `true` if the property is known to be violated.
    pub fn is_violated(&self) -> bool {
        matches!(self, Self::Refuted { .. })
    }

    /// Conservative safety check: safe only if proven.
    /// Unknown states are treated as unsafe (never silent pass).
    pub fn is_conservatively_safe(&self) -> bool {
        matches!(self, Self::Proven)
    }
}

// ─── Safety properties ──────────────────────────────────────────────────────

/// A safety property that can be checked by abstract interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SafetyProperty {
    /// Ordering constraints form a DAG (no circular dependencies).
    EffectOrderingSafety,
    /// No effect writes to any state in the forbidden set.
    NoForbiddenSideEffects {
        /// State IDs that must never be written by any effect.
        forbidden: BTreeSet<IrNodeId>,
    },
    /// All effects on replay-critical paths are marked deterministic.
    DeterminismGuarantee,
    /// Every subscription-model effect has a cleanup strategy other than `None`.
    CleanupCompleteness,
    /// Idempotent effects remain idempotent (no non-idempotent dependency chain).
    IdempotencePreservation,
    /// No two effects write to the same state variable (single-writer rule).
    SingleWriterRule,
}

// ─── Proof obligation & counterexample ──────────────────────────────────────

/// A proof obligation emitted by the analyzer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofObligation {
    /// The safety property this obligation addresses.
    pub property: SafetyProperty,
    /// The effect or IR node under examination.
    pub subject: IrNodeId,
    /// Human-readable description of what must hold.
    pub description: String,
}

/// A counterexample witnessing a property violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counterexample {
    /// The violated property.
    pub property: SafetyProperty,
    /// The chain of effect IDs involved in the violation.
    pub witness_path: Vec<IrNodeId>,
    /// Human-readable explanation.
    pub explanation: String,
}

// ─── Analysis result ────────────────────────────────────────────────────────

/// Complete result of analyzing a set of safety properties.
#[derive(Debug, Clone)]
pub struct AnalysisResult {
    /// Version of the abstract domains used for this analysis.
    pub domain_version: &'static str,
    /// Per-property verification status.
    pub verdicts: BTreeMap<String, PropertyVerdict>,
    /// All proof obligations generated during analysis.
    pub obligations: Vec<ProofObligation>,
    /// All counterexamples found.
    pub counterexamples: Vec<Counterexample>,
    /// Whether all properties passed conservatively.
    pub all_safe: bool,
}

/// Versioned description of one abstract domain and its Galois connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbstractDomainDescriptor {
    /// Shared version tag for this domain contract.
    pub version: &'static str,
    /// Stable domain identifier.
    pub domain_id: &'static str,
    /// Lattice and ordering used by the abstract domain.
    pub lattice: &'static str,
    /// Abstraction map from concrete executions into this domain.
    pub abstraction: &'static str,
    /// Concretization map back to an over-approximated concrete set.
    pub concretization: &'static str,
    /// Soundness condition required by the analyzer.
    pub soundness_contract: &'static str,
    /// Required behavior when the analyzer cannot prove the property.
    pub conservative_fallback: &'static str,
}

/// Return the versioned abstract-domain contracts used by this module.
pub fn abstract_domain_descriptors() -> &'static [AbstractDomainDescriptor] {
    &[
        AbstractDomainDescriptor {
            version: ABSTRACT_INTERPRETATION_DOMAIN_VERSION,
            domain_id: "effect-state-powerset",
            lattice: "powerset(IrNodeId) for may_read, may_write, and executed; order is subset; join is union",
            abstraction: "alpha maps concrete effect executions to their read, write, and executed id sets",
            concretization: "gamma returns all concrete executions whose read, write, and executed sets are subsets of the abstract sets",
            soundness_contract: "for every concrete execution set S, S is a subset of gamma(alpha(S))",
            conservative_fallback: "Unknown is treated as unsafe and never contributes to all_safe",
        },
        AbstractDomainDescriptor {
            version: ABSTRACT_INTERPRETATION_DOMAIN_VERSION,
            domain_id: "ordering-constraint-graph",
            lattice: "directed graph over IrNodeId ordering constraints; order is edge subset; join is edge union",
            abstraction: "alpha maps concrete happens-before constraints to graph edges",
            concretization: "gamma returns all concrete effect schedules compatible with the abstract edge set",
            soundness_contract: "for schedule set S, S is a subset of gamma(alpha(S)); cycle freedom in the abstract graph implies cycle freedom for all represented schedules",
            conservative_fallback: "cycle detection failure or iteration exhaustion must return Unknown or Refuted, never Proven",
        },
        AbstractDomainDescriptor {
            version: ABSTRACT_INTERPRETATION_DOMAIN_VERSION,
            domain_id: "safety-property-verdict",
            lattice: "Proven, Refuted, Unknown with conservative safety order Proven only",
            abstraction: "alpha maps checker outcomes and diagnostics to one property verdict",
            concretization: "gamma maps Proven to all safe executions, Refuted to witnessed violations, and Unknown to untrusted executions",
            soundness_contract: "for verdict set S, S is a subset of gamma(alpha(S)); only Proven may be considered safe by downstream gates",
            conservative_fallback: "Unknown and Refuted both force all_safe=false",
        },
    ]
}

/// Verdict for a single property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyVerdict {
    /// The property that was checked.
    pub property: SafetyProperty,
    /// Verification status.
    pub status: VerificationStatus,
    /// Number of proof obligations generated for this property.
    pub obligation_count: usize,
}

// ─── Abstract effect domain ─────────────────────────────────────────────────

/// Abstract domain for effect state tracking.
///
/// Galois connection:
/// - α: maps concrete effect executions to abstract write/read sets
/// - γ: abstract sets over-approximate all possible concrete executions
///
/// The lattice is (powerset of IrNodeId, ⊆) with join = union, meet = intersection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbstractEffectState {
    /// Over-approximation of state variables that may have been written.
    pub may_write: BTreeSet<IrNodeId>,
    /// Over-approximation of state variables that may have been read.
    pub may_read: BTreeSet<IrNodeId>,
    /// Effects that have been executed in this abstract state.
    pub executed: BTreeSet<IrNodeId>,
}

impl AbstractEffectState {
    /// Bottom element (no effects executed).
    pub fn bottom() -> Self {
        Self {
            may_write: BTreeSet::new(),
            may_read: BTreeSet::new(),
            executed: BTreeSet::new(),
        }
    }

    /// Join (least upper bound) — union of all sets.
    pub fn join(&self, other: &Self) -> Self {
        Self {
            may_write: self.may_write.union(&other.may_write).cloned().collect(),
            may_read: self.may_read.union(&other.may_read).cloned().collect(),
            executed: self.executed.union(&other.executed).cloned().collect(),
        }
    }

    /// Transfer function: abstractly execute an effect.
    pub fn transfer(&self, effect: &CanonicalEffect) -> Self {
        let mut next = self.clone();
        next.may_write.extend(effect.writes.iter().cloned());
        next.may_read.extend(effect.reads.iter().cloned());
        next.executed.insert(effect.id.clone());
        next
    }

    /// Check if this state has reached fixpoint with respect to another.
    pub fn subsumes(&self, other: &Self) -> bool {
        other.may_write.is_subset(&self.may_write)
            && other.may_read.is_subset(&self.may_read)
            && other.executed.is_subset(&self.executed)
    }
}

// ─── Analysis engine ────────────────────────────────────────────────────────

/// Configuration for the abstract interpretation analysis.
#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    /// Maximum fixpoint iterations before declaring Unknown.
    pub max_iterations: usize,
    /// Additional forbidden state IDs (merged with property-specific ones).
    pub global_forbidden_writes: BTreeSet<IrNodeId>,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            max_iterations: 1000,
            global_forbidden_writes: BTreeSet::new(),
        }
    }
}

/// Run abstract interpretation analysis over a canonical effect model.
///
/// Checks all requested safety properties. Returns a comprehensive result
/// with per-property verdicts, proof obligations, and counterexamples.
pub fn analyze_safety(
    model: &CanonicalEffectModel,
    properties: &[SafetyProperty],
    config: &AnalysisConfig,
) -> AnalysisResult {
    let mut verdicts = BTreeMap::new();
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    for property in properties {
        let key = property_key(property);
        let (status, mut obligs, mut cexs) = match property {
            SafetyProperty::EffectOrderingSafety => check_ordering_safety(model),
            SafetyProperty::NoForbiddenSideEffects { forbidden } => {
                let mut combined = forbidden.clone();
                combined.extend(config.global_forbidden_writes.iter().cloned());
                check_no_forbidden_writes(model, &combined)
            }
            SafetyProperty::DeterminismGuarantee => check_determinism(model),
            SafetyProperty::CleanupCompleteness => check_cleanup_completeness(model),
            SafetyProperty::IdempotencePreservation => {
                check_idempotence_preservation(model, config)
            }
            SafetyProperty::SingleWriterRule => check_single_writer(model),
        };

        let obligation_count = obligs.len();
        verdicts.insert(
            key,
            PropertyVerdict {
                property: property.clone(),
                status: status.clone(),
                obligation_count,
            },
        );
        obligations.append(&mut obligs);
        counterexamples.append(&mut cexs);
    }

    let all_safe = verdicts.values().all(|v| v.status.is_conservatively_safe());

    AnalysisResult {
        domain_version: ABSTRACT_INTERPRETATION_DOMAIN_VERSION,
        verdicts,
        obligations,
        counterexamples,
        all_safe,
    }
}

/// Convenience: analyze all standard safety properties.
pub fn analyze_all_safety(model: &CanonicalEffectModel, config: &AnalysisConfig) -> AnalysisResult {
    let properties = vec![
        SafetyProperty::EffectOrderingSafety,
        SafetyProperty::NoForbiddenSideEffects {
            forbidden: config.global_forbidden_writes.clone(),
        },
        SafetyProperty::DeterminismGuarantee,
        SafetyProperty::CleanupCompleteness,
        SafetyProperty::IdempotencePreservation,
        SafetyProperty::SingleWriterRule,
    ];
    analyze_safety(model, &properties, config)
}

// ─── Property checkers ──────────────────────────────────────────────────────

/// Check that ordering constraints form a DAG (no cycles).
fn check_ordering_safety(
    model: &CanonicalEffectModel,
) -> (
    VerificationStatus,
    Vec<ProofObligation>,
    Vec<Counterexample>,
) {
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    // Build adjacency list from ordering constraints.
    let mut graph: BTreeMap<&IrNodeId, BTreeSet<&IrNodeId>> = BTreeMap::new();
    for constraint in &model.ordering_constraints {
        graph
            .entry(&constraint.before)
            .or_default()
            .insert(&constraint.after);
    }

    // For each effect, emit a proof obligation.
    for id in model.effects.keys() {
        obligations.push(ProofObligation {
            property: SafetyProperty::EffectOrderingSafety,
            subject: id.clone(),
            description: format!("Effect '{id}' must not participate in an ordering cycle"),
        });
    }

    // Detect cycles via DFS with coloring.
    // White=0, Gray=1, Black=2
    let mut color: BTreeMap<&IrNodeId, u8> = BTreeMap::new();
    let mut cycle_path: Vec<IrNodeId> = Vec::new();

    fn dfs<'a>(
        node: &'a IrNodeId,
        graph: &BTreeMap<&'a IrNodeId, BTreeSet<&'a IrNodeId>>,
        color: &mut BTreeMap<&'a IrNodeId, u8>,
        path: &mut Vec<&'a IrNodeId>,
    ) -> Option<Vec<IrNodeId>> {
        color.insert(node, 1); // Gray
        path.push(node);

        if let Some(neighbors) = graph.get(node) {
            for &next in neighbors {
                match color.get(next) {
                    Some(1) => {
                        // Back edge → cycle found.
                        let cycle_start = path.iter().position(|&n| n == next).unwrap();
                        return Some(path[cycle_start..].iter().map(|n| (*n).clone()).collect());
                    }
                    Some(2) => {} // Already fully explored.
                    _ => {
                        if let Some(cycle) = dfs(next, graph, color, path) {
                            return Some(cycle);
                        }
                    }
                }
            }
        }

        path.pop();
        color.insert(node, 2); // Black
        None
    }

    for id in model.effects.keys() {
        if color.get(id).copied().unwrap_or(0) == 0 {
            let mut path = Vec::new();
            if let Some(cycle) = dfs(id, &graph, &mut color, &mut path) {
                cycle_path = cycle;
                break;
            }
        }
    }

    let status = if cycle_path.is_empty() {
        VerificationStatus::Proven
    } else {
        let cycle_desc = cycle_path
            .iter()
            .map(|id| id.0.as_str())
            .collect::<Vec<_>>()
            .join(" → ");
        counterexamples.push(Counterexample {
            property: SafetyProperty::EffectOrderingSafety,
            witness_path: cycle_path,
            explanation: format!("Ordering cycle detected: {cycle_desc}"),
        });
        VerificationStatus::Refuted {
            reason: format!("Ordering constraint cycle: {cycle_desc}"),
        }
    };

    (status, obligations, counterexamples)
}

/// Check that no effect writes to forbidden state variables.
fn check_no_forbidden_writes(
    model: &CanonicalEffectModel,
    forbidden: &BTreeSet<IrNodeId>,
) -> (
    VerificationStatus,
    Vec<ProofObligation>,
    Vec<Counterexample>,
) {
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    if forbidden.is_empty() {
        return (VerificationStatus::Proven, obligations, counterexamples);
    }

    for (id, effect) in &model.effects {
        obligations.push(ProofObligation {
            property: SafetyProperty::NoForbiddenSideEffects {
                forbidden: forbidden.clone(),
            },
            subject: id.clone(),
            description: format!(
                "Effect '{}' must not write to forbidden state variables",
                effect.name
            ),
        });

        let violations: BTreeSet<_> = effect.writes.intersection(forbidden).collect();
        if !violations.is_empty() {
            let viol_str = violations
                .iter()
                .map(|v| v.0.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            counterexamples.push(Counterexample {
                property: SafetyProperty::NoForbiddenSideEffects {
                    forbidden: forbidden.clone(),
                },
                witness_path: vec![id.clone()],
                explanation: format!(
                    "Effect '{}' writes to forbidden state: {viol_str}",
                    effect.name
                ),
            });
        }
    }

    let status = if counterexamples.is_empty() {
        VerificationStatus::Proven
    } else {
        VerificationStatus::Refuted {
            reason: format!(
                "{} effect(s) write to forbidden state",
                counterexamples.len()
            ),
        }
    };

    (status, obligations, counterexamples)
}

/// Check that all effects on replay-critical paths are deterministic.
fn check_determinism(
    model: &CanonicalEffectModel,
) -> (
    VerificationStatus,
    Vec<ProofObligation>,
    Vec<Counterexample>,
) {
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    for (id, effect) in &model.effects {
        // Commands and subscriptions that produce messages are replay-critical.
        if effect.execution_model == ExecutionModel::FireAndForget {
            continue;
        }

        obligations.push(ProofObligation {
            property: SafetyProperty::DeterminismGuarantee,
            subject: id.clone(),
            description: format!(
                "Effect '{}' ({:?}) must be deterministic for replay",
                effect.name, effect.execution_model
            ),
        });

        if !effect.deterministic {
            counterexamples.push(Counterexample {
                property: SafetyProperty::DeterminismGuarantee,
                witness_path: vec![id.clone()],
                explanation: format!(
                    "Effect '{}' (kind={:?}) is non-deterministic on a replay-critical path",
                    effect.name, effect.original_kind
                ),
            });
        }
    }

    let status = if counterexamples.is_empty() {
        VerificationStatus::Proven
    } else {
        VerificationStatus::Refuted {
            reason: format!(
                "{} non-deterministic effect(s) on replay-critical paths",
                counterexamples.len()
            ),
        }
    };

    (status, obligations, counterexamples)
}

/// Check that every subscription has a cleanup strategy.
fn check_cleanup_completeness(
    model: &CanonicalEffectModel,
) -> (
    VerificationStatus,
    Vec<ProofObligation>,
    Vec<Counterexample>,
) {
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    for sub_id in &model.subscriptions {
        let effect = match model.effects.get(sub_id) {
            Some(e) => e,
            None => continue,
        };

        obligations.push(ProofObligation {
            property: SafetyProperty::CleanupCompleteness,
            subject: sub_id.clone(),
            description: format!(
                "Subscription '{}' must have a cleanup strategy != None",
                effect.name
            ),
        });

        if effect.cleanup == CleanupStrategy::None {
            counterexamples.push(Counterexample {
                property: SafetyProperty::CleanupCompleteness,
                witness_path: vec![sub_id.clone()],
                explanation: format!(
                    "Subscription '{}' has CleanupStrategy::None; may leak resources",
                    effect.name
                ),
            });
        }
    }

    let status = if counterexamples.is_empty() {
        VerificationStatus::Proven
    } else {
        VerificationStatus::Refuted {
            reason: format!(
                "{} subscription(s) missing cleanup strategy",
                counterexamples.len()
            ),
        }
    };

    (status, obligations, counterexamples)
}

/// Check that idempotent effects don't depend on non-idempotent ones.
fn check_idempotence_preservation(
    model: &CanonicalEffectModel,
    config: &AnalysisConfig,
) -> (
    VerificationStatus,
    Vec<ProofObligation>,
    Vec<Counterexample>,
) {
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    // Build dependency graph from ordering constraints.
    let mut depends_on: BTreeMap<&IrNodeId, BTreeSet<&IrNodeId>> = BTreeMap::new();
    for constraint in &model.ordering_constraints {
        depends_on
            .entry(&constraint.after)
            .or_default()
            .insert(&constraint.before);
    }

    // For each idempotent effect, verify its transitive dependencies are also idempotent.
    for (id, effect) in &model.effects {
        if !effect.idempotent {
            continue;
        }

        obligations.push(ProofObligation {
            property: SafetyProperty::IdempotencePreservation,
            subject: id.clone(),
            description: format!(
                "Idempotent effect '{}' must not depend on non-idempotent effects",
                effect.name
            ),
        });

        // BFS over transitive dependencies.
        let mut visited = BTreeSet::new();
        let mut queue = vec![id];
        let mut iterations = 0;

        while let Some(current) = queue.pop() {
            if iterations >= config.max_iterations {
                return (
                    VerificationStatus::Unknown {
                        reason: format!(
                            "Fixpoint iteration limit ({}) reached checking idempotence of '{}'",
                            config.max_iterations, effect.name
                        ),
                    },
                    obligations,
                    counterexamples,
                );
            }
            iterations += 1;

            if !visited.insert(current) {
                continue;
            }

            if current != id
                && let Some(dep_effect) = model.effects.get(current)
                && !dep_effect.idempotent
            {
                counterexamples.push(Counterexample {
                    property: SafetyProperty::IdempotencePreservation,
                    witness_path: vec![id.clone(), current.clone()],
                    explanation: format!(
                        "Idempotent effect '{}' depends on non-idempotent '{}'",
                        effect.name, dep_effect.name
                    ),
                });
            }

            if let Some(deps) = depends_on.get(current) {
                for dep in deps {
                    queue.push(dep);
                }
            }
        }
    }

    let status = if counterexamples.is_empty() {
        VerificationStatus::Proven
    } else {
        VerificationStatus::Refuted {
            reason: format!(
                "{} idempotence violation(s) in dependency chains",
                counterexamples.len()
            ),
        }
    };

    (status, obligations, counterexamples)
}

/// Check the single-writer rule: no two effects write to the same state variable.
fn check_single_writer(
    model: &CanonicalEffectModel,
) -> (
    VerificationStatus,
    Vec<ProofObligation>,
    Vec<Counterexample>,
) {
    let mut obligations = Vec::new();
    let mut counterexamples = Vec::new();

    // Build state_id → set of effect writers.
    let mut writers: BTreeMap<&IrNodeId, Vec<&IrNodeId>> = BTreeMap::new();
    for (id, effect) in &model.effects {
        obligations.push(ProofObligation {
            property: SafetyProperty::SingleWriterRule,
            subject: id.clone(),
            description: format!(
                "State variables written by '{}' must have no other writers",
                effect.name
            ),
        });

        for state_id in &effect.writes {
            writers.entry(state_id).or_default().push(id);
        }
    }

    // Find state variables with multiple writers.
    for (state_id, effect_ids) in &writers {
        if effect_ids.len() > 1 {
            let writer_names: Vec<_> = effect_ids
                .iter()
                .map(|id| {
                    model
                        .effects
                        .get(*id)
                        .map(|e| e.name.as_str())
                        .unwrap_or("?")
                })
                .collect();
            counterexamples.push(Counterexample {
                property: SafetyProperty::SingleWriterRule,
                witness_path: effect_ids.iter().map(|id| (*id).clone()).collect(),
                explanation: format!(
                    "State '{}' has {} writers: {}",
                    state_id,
                    effect_ids.len(),
                    writer_names.join(", ")
                ),
            });
        }
    }

    let status = if counterexamples.is_empty() {
        VerificationStatus::Proven
    } else {
        VerificationStatus::Refuted {
            reason: format!(
                "{} state variable(s) have multiple writers",
                counterexamples.len()
            ),
        }
    };

    (status, obligations, counterexamples)
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Generate a stable key for a safety property (for verdict map).
fn property_key(property: &SafetyProperty) -> String {
    match property {
        SafetyProperty::EffectOrderingSafety => "effect_ordering_safety".to_string(),
        SafetyProperty::NoForbiddenSideEffects { .. } => "no_forbidden_side_effects".to_string(),
        SafetyProperty::DeterminismGuarantee => "determinism_guarantee".to_string(),
        SafetyProperty::CleanupCompleteness => "cleanup_completeness".to_string(),
        SafetyProperty::IdempotencePreservation => "idempotence_preservation".to_string(),
        SafetyProperty::SingleWriterRule => "single_writer_rule".to_string(),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_canonical::{
        AsyncBoundary, ClassificationConfidence, MessageProtocol, OrderingConstraint,
        TriggerCondition,
    };
    use crate::migration_ir::EffectKind;

    fn make_effect(name: &str, kind: EffectKind, deterministic: bool) -> CanonicalEffect {
        CanonicalEffect {
            id: IrNodeId(format!("ir-{name}")),
            name: name.to_string(),
            original_kind: kind.clone(),
            execution_model: match kind {
                EffectKind::Timer | EffectKind::Subscription | EffectKind::Process => {
                    ExecutionModel::Subscription
                }
                EffectKind::Telemetry => ExecutionModel::FireAndForget,
                _ => ExecutionModel::Command,
            },
            trigger: TriggerCondition::OnMount,
            message_protocol: MessageProtocol::DataResult("test".to_string()),
            cleanup: match kind {
                EffectKind::Timer | EffectKind::Subscription | EffectKind::Process => {
                    CleanupStrategy::SubscriptionStop
                }
                _ => CleanupStrategy::None,
            },
            async_boundary: AsyncBoundary::ThreadPool,
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            dependencies: BTreeSet::new(),
            provenance: crate::migration_ir::Provenance {
                file: "test.rs".to_string(),
                line: 1,
                column: None,
                source_name: None,
                policy_category: None,
            },
            deterministic,
            idempotent: deterministic,
            confidence: ClassificationConfidence {
                score: 0.9,
                rationale: "test".to_string(),
            },
        }
    }

    fn make_model(effects: Vec<CanonicalEffect>) -> CanonicalEffectModel {
        let mut model = CanonicalEffectModel {
            effects: BTreeMap::new(),
            commands: BTreeSet::new(),
            subscriptions: BTreeSet::new(),
            fire_and_forget: BTreeSet::new(),
            ordering_constraints: Vec::new(),
            diagnostics: Vec::new(),
        };
        for effect in effects {
            match effect.execution_model {
                ExecutionModel::Command => {
                    model.commands.insert(effect.id.clone());
                }
                ExecutionModel::Subscription => {
                    model.subscriptions.insert(effect.id.clone());
                }
                ExecutionModel::FireAndForget => {
                    model.fire_and_forget.insert(effect.id.clone());
                }
            }
            model.effects.insert(effect.id.clone(), effect);
        }
        model
    }

    #[test]
    fn ordering_safety_empty_model() {
        let model = make_model(vec![]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::EffectOrderingSafety], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn ordering_safety_no_constraints() {
        let model = make_model(vec![
            make_effect("a", EffectKind::Network, true),
            make_effect("b", EffectKind::Dom, true),
        ]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::EffectOrderingSafety], &config);
        assert!(result.all_safe);
        assert_eq!(result.obligations.len(), 2); // one per effect
    }

    #[test]
    fn ordering_safety_valid_dag() {
        let mut model = make_model(vec![
            make_effect("a", EffectKind::Network, true),
            make_effect("b", EffectKind::Dom, true),
            make_effect("c", EffectKind::Storage, true),
        ]);
        model.ordering_constraints = vec![
            OrderingConstraint {
                before: IrNodeId("ir-a".into()),
                after: IrNodeId("ir-b".into()),
                reason: "a writes, b reads".into(),
            },
            OrderingConstraint {
                before: IrNodeId("ir-b".into()),
                after: IrNodeId("ir-c".into()),
                reason: "b writes, c reads".into(),
            },
        ];
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::EffectOrderingSafety], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn ordering_safety_cycle_detected() {
        let mut model = make_model(vec![
            make_effect("a", EffectKind::Network, true),
            make_effect("b", EffectKind::Dom, true),
        ]);
        model.ordering_constraints = vec![
            OrderingConstraint {
                before: IrNodeId("ir-a".into()),
                after: IrNodeId("ir-b".into()),
                reason: "a before b".into(),
            },
            OrderingConstraint {
                before: IrNodeId("ir-b".into()),
                after: IrNodeId("ir-a".into()),
                reason: "b before a".into(),
            },
        ];
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::EffectOrderingSafety], &config);
        assert!(!result.all_safe);
        assert_eq!(result.counterexamples.len(), 1);
        assert!(!result.counterexamples[0].witness_path.is_empty());
    }

    #[test]
    fn no_forbidden_writes_clean() {
        let mut effect = make_effect("fetch", EffectKind::Network, true);
        effect.writes.insert(IrNodeId("ir-data".into()));
        let model = make_model(vec![effect]);

        let forbidden = BTreeSet::from([IrNodeId("ir-secret".into())]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(
            &model,
            &[SafetyProperty::NoForbiddenSideEffects { forbidden }],
            &config,
        );
        assert!(result.all_safe);
    }

    #[test]
    fn no_forbidden_writes_violation() {
        let mut effect = make_effect("fetch", EffectKind::Network, true);
        effect.writes.insert(IrNodeId("ir-secret".into()));
        let model = make_model(vec![effect]);

        let forbidden = BTreeSet::from([IrNodeId("ir-secret".into())]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(
            &model,
            &[SafetyProperty::NoForbiddenSideEffects { forbidden }],
            &config,
        );
        assert!(!result.all_safe);
        assert_eq!(result.counterexamples.len(), 1);
        assert!(result.counterexamples[0].explanation.contains("ir-secret"));
    }

    #[test]
    fn no_forbidden_writes_empty_forbidden_set() {
        let mut effect = make_effect("fetch", EffectKind::Network, true);
        effect.writes.insert(IrNodeId("ir-anything".into()));
        let model = make_model(vec![effect]);

        let forbidden = BTreeSet::new();
        let config = AnalysisConfig::default();
        let result = analyze_safety(
            &model,
            &[SafetyProperty::NoForbiddenSideEffects { forbidden }],
            &config,
        );
        assert!(result.all_safe);
    }

    #[test]
    fn determinism_all_deterministic() {
        let model = make_model(vec![
            make_effect("a", EffectKind::Dom, true),
            make_effect("b", EffectKind::Storage, true),
        ]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::DeterminismGuarantee], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn determinism_non_deterministic_command() {
        let model = make_model(vec![
            make_effect("fetch", EffectKind::Network, false),
            make_effect("local", EffectKind::Storage, true),
        ]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::DeterminismGuarantee], &config);
        assert!(!result.all_safe);
        assert_eq!(result.counterexamples.len(), 1);
    }

    #[test]
    fn determinism_fire_and_forget_excluded() {
        // Fire-and-forget effects are not on replay-critical paths.
        let model = make_model(vec![make_effect("telemetry", EffectKind::Telemetry, false)]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::DeterminismGuarantee], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn cleanup_completeness_all_clean() {
        let model = make_model(vec![
            make_effect("timer", EffectKind::Timer, true),
            make_effect("sub", EffectKind::Subscription, true),
        ]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::CleanupCompleteness], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn cleanup_completeness_missing_cleanup() {
        let mut sub = make_effect("sub", EffectKind::Subscription, true);
        sub.cleanup = CleanupStrategy::None;
        let model = make_model(vec![sub]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::CleanupCompleteness], &config);
        assert!(!result.all_safe);
        assert_eq!(result.counterexamples.len(), 1);
    }

    #[test]
    fn idempotence_preservation_clean() {
        let model = make_model(vec![
            make_effect("a", EffectKind::Dom, true),
            make_effect("b", EffectKind::Storage, true),
        ]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::IdempotencePreservation], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn idempotence_preservation_violation() {
        let idem = make_effect("cache", EffectKind::Storage, true);
        let non_idem = make_effect("counter", EffectKind::Network, false);
        let mut model = make_model(vec![idem, non_idem]);
        model.ordering_constraints = vec![OrderingConstraint {
            before: IrNodeId("ir-counter".into()),
            after: IrNodeId("ir-cache".into()),
            reason: "counter writes, cache reads".into(),
        }];
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::IdempotencePreservation], &config);
        assert!(!result.all_safe);
    }

    #[test]
    fn single_writer_clean() {
        let mut a = make_effect("a", EffectKind::Dom, true);
        a.writes.insert(IrNodeId("ir-x".into()));
        let mut b = make_effect("b", EffectKind::Dom, true);
        b.writes.insert(IrNodeId("ir-y".into()));
        let model = make_model(vec![a, b]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::SingleWriterRule], &config);
        assert!(result.all_safe);
    }

    #[test]
    fn single_writer_violation() {
        let mut a = make_effect("a", EffectKind::Dom, true);
        a.writes.insert(IrNodeId("ir-shared".into()));
        let mut b = make_effect("b", EffectKind::Network, true);
        b.writes.insert(IrNodeId("ir-shared".into()));
        let model = make_model(vec![a, b]);
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::SingleWriterRule], &config);
        assert!(!result.all_safe);
        assert_eq!(result.counterexamples.len(), 1);
        assert!(result.counterexamples[0].explanation.contains("ir-shared"));
    }

    #[test]
    fn analyze_all_clean_model() {
        let model = make_model(vec![
            make_effect("dom", EffectKind::Dom, true),
            make_effect("timer", EffectKind::Timer, true),
        ]);
        let config = AnalysisConfig::default();
        let result = analyze_all_safety(&model, &config);
        assert!(result.all_safe);
        assert_eq!(result.verdicts.len(), 6); // all 6 standard properties
    }

    #[test]
    fn abstract_state_bottom_and_join() {
        let bottom = AbstractEffectState::bottom();
        assert!(bottom.may_write.is_empty());
        assert!(bottom.may_read.is_empty());
        assert!(bottom.executed.is_empty());

        let mut s1 = AbstractEffectState::bottom();
        s1.may_write.insert(IrNodeId("ir-x".into()));
        let mut s2 = AbstractEffectState::bottom();
        s2.may_read.insert(IrNodeId("ir-y".into()));

        let joined = s1.join(&s2);
        assert!(joined.may_write.contains(&IrNodeId("ir-x".into())));
        assert!(joined.may_read.contains(&IrNodeId("ir-y".into())));
    }

    #[test]
    fn abstract_state_transfer() {
        let state = AbstractEffectState::bottom();
        let mut effect = make_effect("e", EffectKind::Dom, true);
        effect.writes.insert(IrNodeId("ir-out".into()));
        effect.reads.insert(IrNodeId("ir-in".into()));

        let next = state.transfer(&effect);
        assert!(next.may_write.contains(&IrNodeId("ir-out".into())));
        assert!(next.may_read.contains(&IrNodeId("ir-in".into())));
        assert!(next.executed.contains(&IrNodeId("ir-e".into())));
    }

    #[test]
    fn abstract_state_subsumes() {
        let mut s1 = AbstractEffectState::bottom();
        s1.may_write.insert(IrNodeId("ir-x".into()));
        s1.may_write.insert(IrNodeId("ir-y".into()));

        let mut s2 = AbstractEffectState::bottom();
        s2.may_write.insert(IrNodeId("ir-x".into()));

        assert!(s1.subsumes(&s2)); // s1 ⊇ s2
        assert!(!s2.subsumes(&s1)); // s2 ⊅ s1
    }

    #[test]
    fn verification_status_is_safe() {
        assert!(VerificationStatus::Proven.is_safe());
        assert!(!VerificationStatus::Refuted { reason: "x".into() }.is_safe());
        assert!(!VerificationStatus::Unknown { reason: "x".into() }.is_safe());
    }

    #[test]
    fn verification_status_conservative() {
        // Unknown is NOT conservatively safe — this is the key invariant.
        assert!(VerificationStatus::Proven.is_conservatively_safe());
        assert!(
            !VerificationStatus::Unknown {
                reason: "inconclusive".into()
            }
            .is_conservatively_safe()
        );
        assert!(
            !VerificationStatus::Refuted {
                reason: "bad".into()
            }
            .is_conservatively_safe()
        );
    }

    #[test]
    fn global_forbidden_writes_merged() {
        let mut effect = make_effect("fetch", EffectKind::Network, true);
        effect.writes.insert(IrNodeId("ir-global-bad".into()));
        let model = make_model(vec![effect]);

        let mut config = AnalysisConfig::default();
        config
            .global_forbidden_writes
            .insert(IrNodeId("ir-global-bad".into()));

        let result = analyze_safety(
            &model,
            &[SafetyProperty::NoForbiddenSideEffects {
                forbidden: BTreeSet::new(),
            }],
            &config,
        );
        assert!(!result.all_safe);
    }

    #[test]
    fn analysis_result_exposes_domain_version() {
        let model = make_model(vec![make_effect("dom", EffectKind::Dom, true)]);
        let config = AnalysisConfig::default();
        let result = analyze_all_safety(&model, &config);
        assert_eq!(
            result.domain_version,
            ABSTRACT_INTERPRETATION_DOMAIN_VERSION
        );
    }

    #[test]
    fn domain_descriptors_document_versioned_galois_contracts() {
        let descriptors = abstract_domain_descriptors();
        assert!(descriptors.len() >= 3);

        let mut domain_ids = BTreeSet::new();
        for descriptor in descriptors {
            assert_eq!(descriptor.version, ABSTRACT_INTERPRETATION_DOMAIN_VERSION);
            assert!(!descriptor.domain_id.is_empty());
            assert!(
                domain_ids.insert(descriptor.domain_id),
                "domain descriptor ids must be unique"
            );
            assert!(descriptor.abstraction.contains("alpha"));
            assert!(descriptor.concretization.contains("gamma"));
            assert!(descriptor.soundness_contract.contains("gamma(alpha(S))"));
            assert!(descriptor.conservative_fallback.contains("Unknown"));
        }
    }

    #[test]
    fn unknown_fixpoint_state_is_conservative_failure() {
        let model = make_model(vec![make_effect("cache", EffectKind::Storage, true)]);
        let config = AnalysisConfig {
            max_iterations: 0,
            ..AnalysisConfig::default()
        };

        let result = analyze_safety(&model, &[SafetyProperty::IdempotencePreservation], &config);
        assert!(!result.all_safe, "Unknown must not silently pass");
        let verdict = result
            .verdicts
            .get("idempotence_preservation")
            .expect("idempotence verdict should be present");
        assert!(matches!(
            verdict.status,
            VerificationStatus::Unknown { ref reason }
                if reason.contains("Fixpoint iteration limit")
        ));
    }

    #[test]
    fn property_key_unique() {
        let keys: Vec<_> = [
            SafetyProperty::EffectOrderingSafety,
            SafetyProperty::NoForbiddenSideEffects {
                forbidden: BTreeSet::new(),
            },
            SafetyProperty::DeterminismGuarantee,
            SafetyProperty::CleanupCompleteness,
            SafetyProperty::IdempotencePreservation,
            SafetyProperty::SingleWriterRule,
        ]
        .iter()
        .map(property_key)
        .collect();

        let unique: BTreeSet<_> = keys.iter().collect();
        assert_eq!(keys.len(), unique.len(), "property keys must be unique");
    }

    #[test]
    fn three_node_cycle_detected() {
        let mut model = make_model(vec![
            make_effect("a", EffectKind::Dom, true),
            make_effect("b", EffectKind::Dom, true),
            make_effect("c", EffectKind::Dom, true),
        ]);
        model.ordering_constraints = vec![
            OrderingConstraint {
                before: IrNodeId("ir-a".into()),
                after: IrNodeId("ir-b".into()),
                reason: "".into(),
            },
            OrderingConstraint {
                before: IrNodeId("ir-b".into()),
                after: IrNodeId("ir-c".into()),
                reason: "".into(),
            },
            OrderingConstraint {
                before: IrNodeId("ir-c".into()),
                after: IrNodeId("ir-a".into()),
                reason: "".into(),
            },
        ];
        let config = AnalysisConfig::default();
        let result = analyze_safety(&model, &[SafetyProperty::EffectOrderingSafety], &config);
        assert!(!result.all_safe);
        let cex = &result.counterexamples[0];
        assert!(cex.witness_path.len() >= 2);
    }
}
