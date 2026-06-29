//! One-lever-per-change and rollback-readiness policy checker (bd-3bxhj.8.18).
//!
//! Continuous optimization drifts into un-attributable regressions when several
//! levers move in one change window: if three optimizations land together and the
//! benchmark regresses, you cannot tell which one did it, and you cannot cleanly
//! revert. This module enforces two submission-time policies over optimization
//! **change manifests** before they enter the reverse-round governance loop
//! (`reverse_round_governance`, bd-3bxhj.10.17):
//!
//! 1. **One lever per change window** — a change must touch exactly one
//!    optimization lever, *unless* it carries BOTH an explicit policy override
//!    artifact AND a risk waiver (AC1). Anything else is rejected.
//! 2. **Rollback readiness** — every *accepted* change must ship a complete
//!    [`RollbackPlan`]: a revert command, a risk level, and a non-empty
//!    post-rollback validation checklist (AC2).
//!
//! The output is a deterministic ledger of [`ChangePolicyCard`]s, each carrying
//! `change_id`, `lever_count`, `override_flag`, `rollback_plan_id`, and the
//! `rollback_readiness` verdict (AC3), plus a fail-closed gate that blocks any
//! multi-lever or rollback-incomplete submission.
//!
//! The ledger is naturally **float-free** (counts, ids, enums, and booleans only),
//! so it derives [`Eq`] and replays byte-identically.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::RiskTier;

/// Schema version for the one-lever-policy artifacts.
pub const ONE_LEVER_POLICY_SCHEMA_VERSION: &str = "one-lever-policy-v1";

// ── Hashing helpers ──────────────────────────────────────────────────────────

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

// ── Rollback plan schema ─────────────────────────────────────────────────────

/// A rollback plan attached to an optimization change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackPlan {
    /// Stable rollback-plan id.
    pub rollback_plan_id: String,
    /// The command that reverts the change (e.g. a git revert / patch id).
    pub revert_command: String,
    /// The risk level of executing the rollback.
    pub risk_level: RiskTier,
    /// Post-rollback validation commands / checklist (must be non-empty).
    pub post_rollback_validations: Vec<String>,
}

impl RollbackPlan {
    /// Construct a rollback plan.
    #[must_use]
    pub fn new(
        rollback_plan_id: impl Into<String>,
        revert_command: impl Into<String>,
        risk_level: RiskTier,
        post_rollback_validations: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            rollback_plan_id: rollback_plan_id.into(),
            revert_command: revert_command.into(),
            risk_level,
            post_rollback_validations: post_rollback_validations.into_iter().collect(),
        }
    }

    /// Whether the plan is complete: an id, a revert command, and at least one
    /// post-rollback validation step.
    fn is_complete(&self) -> bool {
        !self.rollback_plan_id.is_empty()
            && !self.revert_command.is_empty()
            && !self.post_rollback_validations.is_empty()
            && self.post_rollback_validations.iter().all(|v| !v.is_empty())
    }
}

/// An explicit, audited artifact authorizing a multi-lever change (AC1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyOverride {
    /// Unique artifact id.
    pub artifact_id: String,
    /// Who authored the override.
    pub author: String,
    /// Who approved it.
    pub approved_by: String,
    /// Why multiple levers are bundled into one change.
    pub reason: String,
}

impl PolicyOverride {
    fn is_valid(&self) -> bool {
        !self.artifact_id.is_empty()
            && !self.author.is_empty()
            && !self.approved_by.is_empty()
            && !self.reason.is_empty()
    }
}

/// A risk waiver acknowledging the elevated risk of a multi-lever change (AC1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskWaiver {
    /// Unique waiver id.
    pub waiver_id: String,
    /// The risk level being explicitly accepted.
    pub risk_acknowledged: RiskTier,
    /// The mitigation that makes the elevated risk acceptable.
    pub mitigation: String,
    /// Who approved the waiver.
    pub approved_by: String,
}

impl RiskWaiver {
    fn is_valid(&self) -> bool {
        !self.waiver_id.is_empty() && !self.mitigation.is_empty() && !self.approved_by.is_empty()
    }
}

// ── Change manifest ──────────────────────────────────────────────────────────

/// An optimization change submission (one change window).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationChange {
    /// Stable change id.
    pub change_id: String,
    /// The optimization lever ids touched by this change (one per the policy).
    pub levers: Vec<String>,
    /// The rollback plan, if attached.
    pub rollback_plan: Option<RollbackPlan>,
    /// The policy override, if attached (required for multi-lever).
    pub policy_override: Option<PolicyOverride>,
    /// The risk waiver, if attached (required for multi-lever).
    pub risk_waiver: Option<RiskWaiver>,
}

impl OptimizationChange {
    /// Construct a change touching the given levers.
    #[must_use]
    pub fn new(change_id: impl Into<String>, levers: impl IntoIterator<Item = String>) -> Self {
        Self {
            change_id: change_id.into(),
            levers: levers.into_iter().collect(),
            rollback_plan: None,
            policy_override: None,
            risk_waiver: None,
        }
    }

    /// Attach a rollback plan.
    #[must_use]
    pub fn with_rollback(mut self, plan: RollbackPlan) -> Self {
        self.rollback_plan = Some(plan);
        self
    }

    /// Attach a multi-lever policy override.
    #[must_use]
    pub fn with_override(mut self, over: PolicyOverride) -> Self {
        self.policy_override = Some(over);
        self
    }

    /// Attach a multi-lever risk waiver.
    #[must_use]
    pub fn with_waiver(mut self, waiver: RiskWaiver) -> Self {
        self.risk_waiver = Some(waiver);
        self
    }
}

// ── Verdicts ─────────────────────────────────────────────────────────────────

/// The rollback-readiness verdict for a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackReadiness {
    /// A complete rollback plan is attached.
    Ready,
    /// A rollback plan is attached but incomplete.
    Incomplete,
    /// No rollback plan is attached.
    Missing,
}

impl RollbackReadiness {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Incomplete => "incomplete",
            Self::Missing => "missing",
        }
    }
}

/// One change's policy decision (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePolicyCard {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The change id (AC3 log field).
    pub change_id: String,
    /// The number of levers in the change window (AC3 log field).
    pub lever_count: usize,
    /// Whether a valid policy override is attached (AC3 log field).
    pub override_flag: bool,
    /// Whether a valid risk waiver is attached.
    pub risk_waiver_flag: bool,
    /// The rollback-plan id, if any (AC3 log field).
    pub rollback_plan_id: Option<String>,
    /// The rollback-readiness verdict (AC3 log field).
    pub rollback_readiness: RollbackReadiness,
    /// The rollback risk level, if a plan is attached.
    pub rollback_risk_level: Option<RiskTier>,
    /// Number of post-rollback validation steps.
    pub post_rollback_check_count: usize,
    /// Whether the one-lever policy is satisfied (single lever, or multi + waiver +
    /// override).
    pub one_lever_ok: bool,
    /// Whether the rollback metadata is complete.
    pub rollback_ready: bool,
    /// Whether the change is accepted by the policy gate.
    pub accepted: bool,
    /// The rejection reason (empty when accepted).
    pub rejection_reason: String,
    /// Whether the row's flags are consistent with their recorded data.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn card_has_required_fields(c: &ChangePolicyCard) -> bool {
    !c.schema_version.is_empty()
        && !c.run_id.is_empty()
        && !c.change_id.is_empty()
        && !c.detail.is_empty()
        && !c.reproduction_command.is_empty()
        // AC3: an accepted change must name its rollback plan.
        && (!c.accepted || c.rollback_plan_id.is_some())
        // A rejected change must say why.
        && (c.accepted || !c.rejection_reason.is_empty())
}

/// Evaluate one change against the policy.
fn evaluate_change(run_id: &str, change: &OptimizationChange) -> ChangePolicyCard {
    let lever_count = change.levers.len();
    let override_flag = change
        .policy_override
        .as_ref()
        .is_some_and(PolicyOverride::is_valid);
    let risk_waiver_flag = change
        .risk_waiver
        .as_ref()
        .is_some_and(RiskWaiver::is_valid);

    // AC1: exactly one lever is always allowed; multiple levers need BOTH a valid
    // override and a valid risk waiver; zero levers is an empty (invalid) change.
    let one_lever_ok = match lever_count {
        0 => false,
        1 => true,
        _ => override_flag && risk_waiver_flag,
    };

    let rollback_readiness = match &change.rollback_plan {
        None => RollbackReadiness::Missing,
        Some(plan) if plan.is_complete() => RollbackReadiness::Ready,
        Some(_) => RollbackReadiness::Incomplete,
    };
    let rollback_ready = matches!(rollback_readiness, RollbackReadiness::Ready);

    let accepted = one_lever_ok && rollback_ready;

    let mut reasons: Vec<&str> = Vec::new();
    if lever_count == 0 {
        reasons.push("empty change (no levers)");
    } else if lever_count > 1 && !override_flag {
        reasons.push("multi-lever change without a valid policy override");
    }
    if lever_count > 1 && !risk_waiver_flag {
        reasons.push("multi-lever change without a valid risk waiver");
    }
    match rollback_readiness {
        RollbackReadiness::Missing => reasons.push("no rollback plan attached"),
        RollbackReadiness::Incomplete => {
            reasons.push("rollback plan is incomplete (revert command / checklist)");
        }
        RollbackReadiness::Ready => {}
    }
    let rejection_reason = if accepted {
        String::new()
    } else {
        reasons.join("; ")
    };

    let rollback_plan_id = change
        .rollback_plan
        .as_ref()
        .map(|p| p.rollback_plan_id.clone());
    let rollback_risk_level = change.rollback_plan.as_ref().map(|p| p.risk_level);
    let post_rollback_check_count = change
        .rollback_plan
        .as_ref()
        .map_or(0, |p| p.post_rollback_validations.len());

    // Clause consistency, recomputed from the card's own data:
    //  - accepted ⇔ (one_lever_ok ∧ rollback_ready);
    //  - a multi-lever accepted change carries both flags;
    //  - rollback_ready ⇔ readiness == Ready;
    //  - an accepted change has a non-empty reason exactly when not accepted.
    let accept_consistent = accepted == (one_lever_ok && rollback_ready);
    let multi_lever_consistent =
        !(accepted && lever_count > 1) || (override_flag && risk_waiver_flag);
    let readiness_consistent =
        rollback_ready == matches!(rollback_readiness, RollbackReadiness::Ready);
    let reason_consistent = accepted == rejection_reason.is_empty();
    let clause_consistent =
        accept_consistent && multi_lever_consistent && readiness_consistent && reason_consistent;

    let detail = format!(
        "{} levers={} override={} waiver={} rollback={} -> {}{}",
        change.change_id,
        lever_count,
        override_flag,
        risk_waiver_flag,
        rollback_readiness.as_str(),
        if accepted { "ACCEPTED" } else { "REJECTED" },
        if accepted {
            String::new()
        } else {
            format!(" ({rejection_reason})")
        },
    );

    ChangePolicyCard {
        schema_version: ONE_LEVER_POLICY_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        change_id: change.change_id.clone(),
        lever_count,
        override_flag,
        risk_waiver_flag,
        rollback_plan_id,
        rollback_readiness,
        rollback_risk_level,
        post_rollback_check_count,
        one_lever_ok,
        rollback_ready,
        accepted,
        rejection_reason,
        clause_consistent,
        detail,
        reproduction_command: format!(
            "cargo test -p doctor_frankentui --lib one_lever_policy # change {}",
            change.change_id
        ),
    }
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Roll-up of a one-lever-policy report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OneLeverSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the cards.
    pub evidence_checksum: String,
    /// Total changes evaluated.
    pub total_changes: usize,
    /// Changes accepted.
    pub accepted: usize,
    /// Changes rejected.
    pub rejected: usize,
    /// Multi-lever changes accepted via override + waiver.
    pub multi_lever_exceptions: usize,
    /// Whether every card carries all mandated fields (AC3).
    pub required_fields_complete: bool,
    /// Whether every card's flags match their data.
    pub clauses_consistent: bool,
    /// AC1: no multi-lever change is accepted without both an override and a
    /// waiver (re-derived independently).
    pub one_lever_enforced: bool,
    /// AC2: every accepted change ships a complete rollback plan.
    pub rollback_enforced: bool,
    /// AC3: every card logs change_id / lever_count / override_flag /
    /// rollback_plan_id / rollback_readiness.
    pub logs_complete: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OneLeverStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full one-lever-policy report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OneLeverReport {
    /// Per-change policy cards.
    pub cards: Vec<ChangePolicyCard>,
    /// The accepted change ids (sorted).
    pub accepted_change_ids: Vec<String>,
    /// The roll-up summary + gate.
    pub summary: OneLeverSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: OneLeverStatsArtifact,
}

impl OneLeverReport {
    /// Look up a card by change id.
    #[must_use]
    pub fn card(&self, change_id: &str) -> Option<&ChangePolicyCard> {
        self.cards.iter().find(|c| c.change_id == change_id)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct Checksummed<'a> {
    cards: &'a [ChangePolicyCard],
}

/// Compile a full one-lever-policy report over `changes`.
#[must_use]
pub fn run_one_lever_policy(label: &str, changes: &[OptimizationChange]) -> OneLeverReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{ONE_LEVER_POLICY_SCHEMA_VERSION}|{label}"
    )));

    let cards: Vec<ChangePolicyCard> = changes
        .iter()
        .map(|c| evaluate_change(&run_id, c))
        .collect();

    let mut accepted_change_ids: Vec<String> = cards
        .iter()
        .filter(|c| c.accepted)
        .map(|c| c.change_id.clone())
        .collect();
    accepted_change_ids.sort();

    let evidence_checksum = stable_hash(&Checksummed { cards: &cards });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let accepted = cards.iter().filter(|c| c.accepted).count();
    let rejected = cards.len() - accepted;
    let multi_lever_exceptions = cards
        .iter()
        .filter(|c| c.accepted && c.lever_count > 1)
        .count();

    let required_fields_complete = cards.iter().all(card_has_required_fields);
    let clauses_consistent = cards.iter().all(|c| c.clause_consistent);
    // AC1: re-derive — no accepted multi-lever change lacks an override + waiver.
    let one_lever_enforced = cards
        .iter()
        .all(|c| !c.accepted || c.lever_count == 1 || (c.override_flag && c.risk_waiver_flag));
    // AC2: every accepted change ships a complete (Ready) rollback plan.
    let rollback_enforced = cards.iter().all(|c| {
        !c.accepted
            || (matches!(c.rollback_readiness, RollbackReadiness::Ready)
                && c.rollback_plan_id.is_some())
    });
    // AC3: every card carries the mandated log fields.
    let logs_complete = cards.iter().all(|c| {
        !c.change_id.is_empty()
            // lever_count, override_flag, rollback_readiness are always present;
            // an accepted change must name its rollback plan.
            && (!c.accepted || c.rollback_plan_id.is_some())
    });

    let gate_passes = required_fields_complete
        && clauses_consistent
        && one_lever_enforced
        && rollback_enforced
        && logs_complete;

    let summary = OneLeverSummary {
        schema_version: ONE_LEVER_POLICY_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_changes: cards.len(),
        accepted,
        rejected,
        multi_lever_exceptions,
        required_fields_complete,
        clauses_consistent,
        one_lever_enforced,
        rollback_enforced,
        logs_complete,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib one_lever_policy # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a OneLeverSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: ONE_LEVER_POLICY_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        OneLeverStatsArtifact {
            path: format!("one_lever_policy/{report_id}.json"),
            sha256,
            content,
        }
    };

    OneLeverReport {
        cards,
        accepted_change_ids,
        summary,
        exported_json_stats,
    }
}

/// A complete, ready rollback plan for the corpus.
fn ready_plan(id: &str) -> RollbackPlan {
    RollbackPlan::new(
        id,
        "git revert <sha>",
        RiskTier::Low,
        [
            "cargo test -p ftui-render --lib".to_string(),
            "./scripts/perf_gate.sh".to_string(),
        ],
    )
}

/// A representative corpus of optimization changes spanning accept / reject /
/// exception outcomes.
#[must_use]
pub fn default_optimization_changes() -> Vec<OptimizationChange> {
    vec![
        // Single lever + complete rollback: accepted.
        OptimizationChange::new("chg.single_ok", ["lever.render_diff_simd".to_string()])
            .with_rollback(ready_plan("rbk.single")),
        // Single lever, no rollback plan: rejected (AC2).
        OptimizationChange::new("chg.no_rollback", ["lever.layout_smallvec".to_string()]),
        // Multi-lever, no override / waiver: rejected (AC1).
        OptimizationChange::new(
            "chg.multi_bare",
            [
                "lever.a".to_string(),
                "lever.b".to_string(),
                "lever.c".to_string(),
            ],
        )
        .with_rollback(ready_plan("rbk.multi_bare")),
        // Multi-lever WITH override + waiver + rollback: accepted exception.
        OptimizationChange::new(
            "chg.multi_waived",
            ["lever.x".to_string(), "lever.y".to_string()],
        )
        .with_rollback(ready_plan("rbk.multi_waived"))
        .with_override(PolicyOverride {
            artifact_id: "ovr-9".to_string(),
            author: "perf-team".to_string(),
            approved_by: "tech-lead".to_string(),
            reason: "two levers are interdependent and must land atomically".to_string(),
        })
        .with_waiver(RiskWaiver {
            waiver_id: "wvr-9".to_string(),
            risk_acknowledged: RiskTier::High,
            mitigation: "shadow-run validated + staged rollout".to_string(),
            approved_by: "tech-lead".to_string(),
        }),
        // Single lever but an incomplete rollback plan (no checklist): rejected.
        OptimizationChange::new("chg.bad_rollback", ["lever.z".to_string()]).with_rollback(
            RollbackPlan::new("rbk.bad", "git revert <sha>", RiskTier::Medium, []),
        ),
    ]
}

/// Run the default one-lever-policy report.
#[must_use]
pub fn run_default_one_lever_report(label: &str) -> OneLeverReport {
    run_one_lever_policy(label, &default_optimization_changes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_lever_with_rollback_is_accepted() {
        let change =
            OptimizationChange::new("c", ["lever.a".to_string()]).with_rollback(ready_plan("rbk"));
        let card = evaluate_change("run", &change);
        assert!(card.one_lever_ok);
        assert!(card.rollback_ready);
        assert!(card.accepted);
        assert_eq!(card.rollback_readiness, RollbackReadiness::Ready);
        assert!(card.rejection_reason.is_empty());
        assert!(card.clause_consistent);
    }

    #[test]
    fn single_lever_without_rollback_is_rejected() {
        let change = OptimizationChange::new("c", ["lever.a".to_string()]);
        let card = evaluate_change("run", &change);
        assert!(card.one_lever_ok);
        assert!(!card.rollback_ready);
        assert!(!card.accepted);
        assert_eq!(card.rollback_readiness, RollbackReadiness::Missing);
        assert!(card.rejection_reason.contains("rollback"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn multi_lever_without_override_is_rejected() {
        let change = OptimizationChange::new("c", ["lever.a".to_string(), "lever.b".to_string()])
            .with_rollback(ready_plan("rbk"));
        let card = evaluate_change("run", &change);
        assert_eq!(card.lever_count, 2);
        assert!(!card.one_lever_ok);
        assert!(!card.accepted);
        assert!(card.rejection_reason.contains("override"));
        assert!(card.rejection_reason.contains("waiver"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn multi_lever_with_override_and_waiver_is_accepted() {
        let change = OptimizationChange::new("c", ["lever.a".to_string(), "lever.b".to_string()])
            .with_rollback(ready_plan("rbk"))
            .with_override(PolicyOverride {
                artifact_id: "o".to_string(),
                author: "a".to_string(),
                approved_by: "b".to_string(),
                reason: "atomic".to_string(),
            })
            .with_waiver(RiskWaiver {
                waiver_id: "w".to_string(),
                risk_acknowledged: RiskTier::High,
                mitigation: "shadow-run".to_string(),
                approved_by: "b".to_string(),
            });
        let card = evaluate_change("run", &change);
        assert!(card.override_flag);
        assert!(card.risk_waiver_flag);
        assert!(card.one_lever_ok);
        assert!(card.accepted);
        assert!(card.clause_consistent);
    }

    #[test]
    fn multi_lever_with_override_but_no_waiver_is_rejected() {
        let change = OptimizationChange::new("c", ["lever.a".to_string(), "lever.b".to_string()])
            .with_rollback(ready_plan("rbk"))
            .with_override(PolicyOverride {
                artifact_id: "o".to_string(),
                author: "a".to_string(),
                approved_by: "b".to_string(),
                reason: "atomic".to_string(),
            });
        let card = evaluate_change("run", &change);
        assert!(card.override_flag);
        assert!(!card.risk_waiver_flag);
        assert!(!card.one_lever_ok);
        assert!(!card.accepted);
        assert!(card.rejection_reason.contains("waiver"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn incomplete_rollback_plan_is_rejected() {
        let change = OptimizationChange::new("c", ["lever.a".to_string()])
            .with_rollback(RollbackPlan::new("rbk", "git revert", RiskTier::Low, []));
        let card = evaluate_change("run", &change);
        assert_eq!(card.rollback_readiness, RollbackReadiness::Incomplete);
        assert!(!card.accepted);
        assert!(card.rejection_reason.contains("incomplete"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn empty_change_is_rejected() {
        let change = OptimizationChange::new("c", std::iter::empty::<String>())
            .with_rollback(ready_plan("rbk"));
        let card = evaluate_change("run", &change);
        assert_eq!(card.lever_count, 0);
        assert!(!card.one_lever_ok);
        assert!(!card.accepted);
        assert!(card.rejection_reason.contains("empty"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn malformed_override_does_not_authorize_multi_lever() {
        // Missing approver on the override: invalid, so the multi-lever change is
        // not authorized even though a waiver is present.
        let change = OptimizationChange::new("c", ["lever.a".to_string(), "lever.b".to_string()])
            .with_rollback(ready_plan("rbk"))
            .with_override(PolicyOverride {
                artifact_id: "o".to_string(),
                author: "a".to_string(),
                approved_by: String::new(),
                reason: "atomic".to_string(),
            })
            .with_waiver(RiskWaiver {
                waiver_id: "w".to_string(),
                risk_acknowledged: RiskTier::High,
                mitigation: "shadow-run".to_string(),
                approved_by: "b".to_string(),
            });
        let card = evaluate_change("run", &change);
        assert!(!card.override_flag);
        assert!(!card.one_lever_ok);
        assert!(!card.accepted);
    }

    #[test]
    fn ledger_is_replay_stable() {
        let a = run_default_one_lever_report("olp/test");
        let b = run_default_one_lever_report("olp/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.cards, b.cards);
    }

    #[test]
    fn empty_report_passes_gate() {
        let report = run_one_lever_policy("olp/empty", &[]);
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_changes, 0);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_one_lever_report("olp/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_changes, 5);
        assert_eq!(report.summary.accepted, 2);
        assert_eq!(report.summary.rejected, 3);
        assert_eq!(report.summary.multi_lever_exceptions, 1);
        assert!(report.summary.one_lever_enforced);
        assert!(report.summary.rollback_enforced);
        assert!(report.summary.logs_complete);
        // The accepted set is exactly the single-ok and the waived multi-lever.
        assert_eq!(
            report.accepted_change_ids,
            vec!["chg.multi_waived".to_string(), "chg.single_ok".to_string()]
        );
        for c in &report.cards {
            assert!(card_has_required_fields(c));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_one_lever_report("olp/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
