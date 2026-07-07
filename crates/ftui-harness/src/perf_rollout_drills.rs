#![forbid(unsafe_code)]

//! Shadow, canary, fallback, rollback, and recovery drills for
//! performance-sensitive changes (bd-lilcl).
//!
//! Benchmarks tell you a change is fast; drills tell you the team can
//! OPERATE it. Each drill here is an executable scenario — not aspirational
//! prose — that exercises one rollout action end to end, including its
//! failure path, and emits both an operator-readable summary and
//! machine-readable evidence that joins the perf-evidence-ledger /
//! promotion-scorecard vocabulary.
//!
//! # The five drills and the risk each one controls
//!
//! | Drill | Question it answers | Risk controlled |
//! |-------|--------------------|-----------------|
//! | Shadow | does the candidate behave like the baseline off the hot path? | shipping a regression that side-by-side comparison would have caught |
//! | Canary | does the win survive widening real exposure? | curated-suite overfitting reaching all users at once |
//! | Fallback | when monitors trip mid-flight, does disabling the change restore the envelope? | being stuck degraded with no safe exit |
//! | Rollback | can a previously promoted change be demoted on new evidence? | promotion treated as irreversible |
//! | Recovery | after an incident, are the artifacts complete and reproducible? | learning nothing from a failed optimization |
//!
//! # Failure paths are first-class
//!
//! [`standard_drill_suite`] runs every drill through BOTH its clean scenario
//! and its failure scenario. A canary drill that aborts stage 2 on a tail
//! hard-fail is the drill *succeeding at its job* — `mechanism_ok` tracks
//! whether the drill machinery behaved correctly, independent of whether the
//! simulated change was good.
//!
//! # Connection to the ledger and the scorecard
//!
//! Drill evidence embeds [`crate::tail_regime_monitor`] report JSON and
//! [`crate::promotion_scorecard`] decision JSON verbatim, so a drill artifact
//! is navigable with the same tooling as any other perf evidence
//! (`tail-regime-monitor-v1`, `perf-promotion-scorecard-v1`). Rollout actions
//! map to the scorecard's gate actions: `proceed` → widen, `proceed_with_review`
//! → hold at current exposure, `block_rollout` → fall back / roll back.
//!
//! # When artifacts disagree or monitors trip unexpectedly
//!
//! The recovery drill encodes the playbook as operator guidance in its
//! report: re-run the evaluation from raw samples (all verdicts here are
//! deterministic and byte-identical on replay); a stored report that
//! disagrees with a replayed evaluation means the STORED artifact is
//! untrusted — regenerate it and file a defect against the pipeline that
//! wrote it. An unexpected tail-monitor trip during canary is always resolved
//! toward safety: hold or reduce exposure first, diagnose second; the
//! evidence bundle preserves the tripped report so nothing is lost by
//! falling back early.

use crate::promotion_scorecard::{
    CategoryEvidence, EvidenceCategory, EvidenceStatus, PromotionEvidence, PromotionScorecard,
    PromotionTier, TierVerdict, ValueDeltas,
};
use crate::tail_regime_monitor::{MetricSeries, MetricUnit, TailRegimeMonitor, Verdict};
use crate::validation_matrix::PerfLane;

/// Schema version for drill reports.
pub const PERF_ROLLOUT_DRILLS_SCHEMA_VERSION: &str = "perf-rollout-drills-v1";

// ============================================================================
// Drill vocabulary
// ============================================================================

/// The five rollout drills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DrillKind {
    /// Side-by-side baseline/candidate comparison off the hot path.
    Shadow,
    /// Staged exposure with monitor checks at each widening step.
    Canary,
    /// Mid-flight monitor trip → disable change → verify envelope restored.
    Fallback,
    /// Demote a previously promoted change on new failing evidence.
    Rollback,
    /// Post-incident artifact completeness + reproducibility audit.
    Recovery,
}

impl DrillKind {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Canary => "canary",
            Self::Fallback => "fallback",
            Self::Rollback => "rollback",
            Self::Recovery => "recovery",
        }
    }

    /// The risk this drill controls — why it exists (bead AC #3).
    #[must_use]
    pub const fn risk_controlled(self) -> &'static str {
        match self {
            Self::Shadow => {
                "shipping a regression that a side-by-side comparison would have caught"
            }
            Self::Canary => "curated-suite overfitting reaching every user at once",
            Self::Fallback => "being stuck in a degraded state with no safe exit",
            Self::Rollback => "treating promotion as irreversible once defaults flip",
            Self::Recovery => "learning nothing from a failed optimization incident",
        }
    }
}

/// One structured step in a drill's decision path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrillStep {
    /// What happened (stable snake_case action).
    pub action: String,
    /// Verdict/observation at this step.
    pub observation: String,
    /// Embedded machine evidence (monitor/scorecard JSON), when produced.
    pub evidence_json: Option<String>,
}

/// Executable drill result: operator summary + machine evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrillReport {
    /// Which drill ran.
    pub kind: DrillKind,
    /// Scenario name (clean vs failure path).
    pub scenario: &'static str,
    /// Did the drill MACHINERY behave correctly? (A canary that aborts on a
    /// bad candidate has `mechanism_ok = true`.)
    pub mechanism_ok: bool,
    /// Ordered decision path with embedded evidence.
    pub steps: Vec<DrillStep>,
    /// What an operator should do next, and why.
    pub operator_guidance: Vec<String>,
}

impl DrillReport {
    /// Machine-readable JSON (single line, deterministic).
    #[must_use]
    pub fn to_json(&self) -> String {
        let steps = self
            .steps
            .iter()
            .map(|s| {
                format!(
                    "{{\"action\":\"{}\",\"observation\":\"{}\",\"evidence\":{}}}",
                    escape_json(&s.action),
                    escape_json(&s.observation),
                    s.evidence_json
                        .as_ref()
                        .map_or_else(|| "null".to_string(), |e| e.clone()),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let guidance = self
            .operator_guidance
            .iter()
            .map(|g| format!("\"{}\"", escape_json(g)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema_version\":\"{}\",\"drill\":\"{}\",\"scenario\":\"{}\",\"risk_controlled\":\"{}\",\"mechanism_ok\":{},\"steps\":[{}],\"operator_guidance\":[{}]}}",
            PERF_ROLLOUT_DRILLS_SCHEMA_VERSION,
            self.kind.as_str(),
            self.scenario,
            escape_json(self.kind.risk_controlled()),
            self.mechanism_ok,
            steps,
            guidance,
        )
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ============================================================================
// Drill engine
// ============================================================================

/// Drill engine binding the tail monitors and the promotion scorecard.
#[derive(Debug, Clone, Default)]
pub struct RolloutDrillEngine {
    monitor: TailRegimeMonitor,
    scorecard: PromotionScorecard,
}

impl RolloutDrillEngine {
    /// Shadow drill: compare candidate against baseline off the hot path.
    ///
    /// Clean path → guidance to proceed to canary. Regression → guidance to
    /// stop before any user exposure, with the monitor report preserved.
    #[must_use]
    pub fn run_shadow_drill(
        &self,
        scenario: &'static str,
        baseline: &MetricSeries,
        candidate: &MetricSeries,
    ) -> DrillReport {
        let report = self.monitor.evaluate(baseline, candidate);
        let json = report.to_json();
        let mut steps = vec![DrillStep {
            action: "shadow_compare".to_string(),
            observation: format!(
                "monitor overall={} gate_action={}",
                report.overall.as_str(),
                report.gate_action()
            ),
            evidence_json: Some(json),
        }];
        let guidance = match report.overall {
            Verdict::Pass => vec![
                "shadow comparison clean: proceed to the canary drill; do NOT skip staged \
                 exposure on the strength of shadow results alone (shadow load is not user load)"
                    .to_string(),
            ],
            Verdict::Warn => vec![
                "shadow comparison shows warnings: review the named findings before canarying; \
                 a warning here becomes a hard failure under real load more often than not"
                    .to_string(),
            ],
            Verdict::HardFail => vec![
                "shadow comparison hard-failed: stop before any user exposure; file the \
                 embedded monitor report against the change and re-run after a fix — the whole \
                 point of shadow is that this costs nothing to catch here"
                    .to_string(),
            ],
        };
        steps.push(DrillStep {
            action: "decide_next_stage".to_string(),
            observation: if report.overall == Verdict::Pass {
                "advance_to_canary".to_string()
            } else {
                "halt_before_exposure".to_string()
            },
            evidence_json: None,
        });
        DrillReport {
            kind: DrillKind::Shadow,
            scenario,
            mechanism_ok: true,
            steps,
            operator_guidance: guidance,
        }
    }

    /// Canary drill: widen exposure stage by stage, checking monitors at
    /// each step. A hard failure aborts (revert exposure); a warning HOLDS —
    /// exposure must not widen past a stage that needs review, so later
    /// stages are not evaluated.
    #[must_use]
    pub fn run_canary_drill(
        &self,
        scenario: &'static str,
        baseline: &MetricSeries,
        stages: &[(&'static str, MetricSeries)],
    ) -> DrillReport {
        let mut steps = Vec::new();
        let mut aborted_at: Option<&'static str> = None;
        let mut held_at: Option<&'static str> = None;
        for (stage_name, stage_series) in stages {
            let report = self.monitor.evaluate(baseline, stage_series);
            let verdict = report.overall;
            steps.push(DrillStep {
                action: format!("canary_stage_{stage_name}"),
                observation: format!(
                    "monitor overall={} gate_action={}",
                    verdict.as_str(),
                    report.gate_action()
                ),
                evidence_json: Some(report.to_json()),
            });
            match verdict {
                Verdict::HardFail => {
                    aborted_at = Some(stage_name);
                    steps.push(DrillStep {
                        action: "canary_abort".to_string(),
                        observation: format!("aborted_at_stage_{stage_name}"),
                        evidence_json: None,
                    });
                    break;
                }
                Verdict::Warn => {
                    held_at = Some(stage_name);
                    steps.push(DrillStep {
                        action: "canary_hold".to_string(),
                        observation: format!("held_at_stage_{stage_name}"),
                        evidence_json: None,
                    });
                    break;
                }
                Verdict::Pass => {}
            }
        }
        let guidance = if let Some(stage) = aborted_at {
            vec![
                format!(
                    "canary aborted at stage {stage}: revert exposure to the previous stage \
                     immediately, keep the tripped monitor report in the incident bundle, and \
                     route the change back through shadow after a fix"
                ),
                "an unexpected monitor trip during canary is always resolved toward safety: \
                 reduce exposure first, diagnose second — the evidence is preserved either way"
                    .to_string(),
            ]
        } else if let Some(stage) = held_at {
            vec![format!(
                "canary held at stage {stage}: exposure stays at this stage until the warning \
                 findings are reviewed; widening past a stage that needs review is never \
                 automatic"
            )]
        } else {
            vec![
                "all canary stages within envelope: eligible for promotion scoring; attach the \
                 per-stage monitor reports as the robustness evidence row"
                    .to_string(),
            ]
        };
        DrillReport {
            kind: DrillKind::Canary,
            scenario,
            mechanism_ok: true,
            steps,
            operator_guidance: guidance,
        }
    }

    /// Fallback drill: the change trips monitors mid-flight; disabling it
    /// must restore the trusted envelope.
    #[must_use]
    pub fn run_fallback_drill(
        &self,
        scenario: &'static str,
        baseline: &MetricSeries,
        tripped: &MetricSeries,
        post_fallback: &MetricSeries,
    ) -> DrillReport {
        let trip_report = self.monitor.evaluate(baseline, tripped);
        let restored_report = self.monitor.evaluate(baseline, post_fallback);
        let restored = restored_report.overall == Verdict::Pass;
        let steps = vec![
            DrillStep {
                action: "monitor_trip_detected".to_string(),
                observation: format!("overall={}", trip_report.overall.as_str()),
                evidence_json: Some(trip_report.to_json()),
            },
            DrillStep {
                action: "disable_change_fallback".to_string(),
                observation: "optimized path disabled; baseline path active".to_string(),
                evidence_json: None,
            },
            DrillStep {
                action: "verify_envelope_restored".to_string(),
                observation: format!("post-fallback overall={}", restored_report.overall.as_str()),
                evidence_json: Some(restored_report.to_json()),
            },
        ];
        let guidance = if restored {
            vec![
                "fallback restored the trusted envelope: keep the change disabled, attach both \
                 monitor reports to the incident, and require a fresh shadow+canary pass before \
                 re-enabling"
                    .to_string(),
            ]
        } else {
            vec![
                "post-fallback metrics are STILL outside the envelope: the fallback path itself \
                 is compromised or the regression was never caused by this change — escalate to \
                 incident response, do not iterate on the optimization until the baseline is \
                 healthy again"
                    .to_string(),
            ]
        };
        DrillReport {
            kind: DrillKind::Fallback,
            scenario,
            // The drill machinery is correct iff the tripped series was
            // actually detected AND the restoration verdict is coherent.
            mechanism_ok: trip_report.overall == Verdict::HardFail,
            steps,
            operator_guidance: guidance,
        }
    }

    /// Rollback drill: new failing evidence demotes a previously promoted
    /// change through the scorecard, and the post-rollback series must sit
    /// back inside the envelope.
    #[must_use]
    pub fn run_rollback_drill(
        &self,
        scenario: &'static str,
        promoted_evidence: &PromotionEvidence,
        failing_evidence: &PromotionEvidence,
        baseline: &MetricSeries,
        post_rollback: &MetricSeries,
    ) -> DrillReport {
        let before = self.scorecard.evaluate(promoted_evidence);
        let after = self.scorecard.evaluate(failing_evidence);
        let was_promotable = before.highest_admissible >= Some(PromotionTier::CiAcceptance);
        let now_blocked = after
            .tiers
            .iter()
            .find(|t| t.tier == PromotionTier::DefaultEnablement)
            .is_some_and(|t| t.verdict == TierVerdict::NoGo);
        let restored_report = self.monitor.evaluate(baseline, post_rollback);
        let restored = restored_report.overall == Verdict::Pass;

        let steps = vec![
            DrillStep {
                action: "promotion_state_before".to_string(),
                observation: format!("highest_admissible={:?}", before.highest_admissible),
                evidence_json: Some(before.to_json()),
            },
            DrillStep {
                action: "rescore_with_new_evidence".to_string(),
                observation: if now_blocked {
                    "default_enablement=no_go -> rollback required".to_string()
                } else {
                    "new evidence did not demote the change".to_string()
                },
                evidence_json: Some(after.to_json()),
            },
            DrillStep {
                action: "execute_rollback".to_string(),
                observation: "default flipped back to the baseline path".to_string(),
                evidence_json: None,
            },
            DrillStep {
                action: "verify_envelope_restored".to_string(),
                observation: format!("post-rollback overall={}", restored_report.overall.as_str()),
                evidence_json: Some(restored_report.to_json()),
            },
        ];
        DrillReport {
            kind: DrillKind::Rollback,
            scenario,
            mechanism_ok: was_promotable && now_blocked && restored,
            steps,
            operator_guidance: vec![
                "promotion is reversible by design: the same scorecard that admitted the change \
                 demotes it when the evidence changes — never hand-edit defaults around the \
                 policy"
                    .to_string(),
                "keep BOTH promotion decisions (before/after) in the incident bundle; the delta \
                 between them is the regression's paper trail"
                    .to_string(),
            ],
        }
    }

    /// Recovery drill: after a failed optimization, the evidence must be
    /// complete AND reproducible — re-running the deterministic evaluations
    /// must reproduce the stored artifacts byte for byte.
    #[must_use]
    pub fn run_recovery_drill(
        &self,
        scenario: &'static str,
        baseline: &MetricSeries,
        incident_candidate: &MetricSeries,
        stored_report_json: &str,
    ) -> DrillReport {
        let replayed = self
            .monitor
            .evaluate(baseline, incident_candidate)
            .to_json();
        let reproducible = replayed == stored_report_json;
        let steps = vec![
            DrillStep {
                action: "replay_evaluation_from_raw_samples".to_string(),
                observation: if reproducible {
                    "replayed report is byte-identical to the stored artifact".to_string()
                } else {
                    "replayed report DISAGREES with the stored artifact".to_string()
                },
                evidence_json: Some(replayed),
            },
            DrillStep {
                action: "artifact_trust_decision".to_string(),
                observation: if reproducible {
                    "stored artifact trusted".to_string()
                } else {
                    "stored artifact quarantined; replayed evaluation is authoritative".to_string()
                },
                evidence_json: None,
            },
        ];
        let guidance = if reproducible {
            vec![
                "artifacts are complete and reproducible: proceed with the post-incident review \
                 using the stored bundle"
                    .to_string(),
            ]
        } else {
            vec![
                "when artifacts disagree, the replayed evaluation from raw samples wins: all \
                 verdicts in this pipeline are deterministic, so divergence means the stored \
                 artifact (or the pipeline that wrote it) is defective — regenerate the bundle \
                 and file a defect against the writer"
                    .to_string(),
            ]
        };
        DrillReport {
            kind: DrillKind::Recovery,
            scenario,
            mechanism_ok: true,
            steps,
            operator_guidance: guidance,
        }
    }
}

// ============================================================================
// Standard deterministic drill suite (clean + failure path per drill)
// ============================================================================

fn series(samples: Vec<u64>) -> MetricSeries {
    MetricSeries::new(
        PerfLane::Render,
        "frame_time_us",
        MetricUnit::Micros,
        samples,
    )
}

fn healthy() -> Vec<u64> {
    (0..60).map(|i| 100 + (i % 10)).collect()
}

fn regressed() -> Vec<u64> {
    (0..60).map(|i| 140 + (i % 10)).collect()
}

fn evidence(change_id: &str, all_passing: bool) -> PromotionEvidence {
    PromotionEvidence {
        change_id: change_id.to_string(),
        categories: EvidenceCategory::ALL
            .iter()
            .map(|&category| CategoryEvidence {
                category,
                status: if all_passing || category != EvidenceCategory::TailMonitors {
                    EvidenceStatus::Passing
                } else {
                    EvidenceStatus::Failing
                },
                summary: format!("{} evidence", category.as_str()),
                ledger_refs: vec![format!("ledger-{}", category.as_str())],
            })
            .collect(),
        value: Some(ValueDeltas {
            avg_delta_permille: -30,
            tail_delta_permille: -60,
        }),
    }
}

/// Run every drill through its clean AND failure scenario with fixed
/// deterministic fixtures. This is what the E2E script executes.
#[must_use]
pub fn standard_drill_suite() -> Vec<DrillReport> {
    let engine = RolloutDrillEngine::default();
    let base = series(healthy());
    let clean = series((0..60).map(|i| 100 + ((i + 3) % 10)).collect());
    let bad = series(regressed());

    // Shadow: clean + regression.
    let mut reports = vec![
        engine.run_shadow_drill("shadow_clean", &base, &clean),
        engine.run_shadow_drill("shadow_regression", &base, &bad),
    ];

    // Canary: promotes through three widening stages / aborts at stage 2.
    reports.push(engine.run_canary_drill(
        "canary_promotes",
        &base,
        &[
            (
                "10pct",
                series((0..60).map(|i| 100 + ((i + 1) % 10)).collect()),
            ),
            (
                "50pct",
                series((0..60).map(|i| 100 + ((i + 5) % 10)).collect()),
            ),
            (
                "100pct",
                series((0..60).map(|i| 100 + ((i + 7) % 10)).collect()),
            ),
        ],
    ));
    reports.push(engine.run_canary_drill(
        "canary_aborts_on_widening",
        &base,
        &[
            (
                "10pct",
                series((0..60).map(|i| 100 + ((i + 1) % 10)).collect()),
            ),
            ("50pct", series(regressed())),
            ("100pct", series(regressed())),
        ],
    ));

    // Fallback: trip mid-flight, restore envelope.
    reports.push(engine.run_fallback_drill("fallback_restores_envelope", &base, &bad, &clean));

    // Rollback: promoted change demoted on new evidence; envelope restored.
    reports.push(engine.run_rollback_drill(
        "rollback_demotes_promoted_change",
        &evidence("opt-change-7", true),
        &evidence("opt-change-7", false),
        &base,
        &clean,
    ));

    // Recovery: reproducible artifact + a tampered/stale artifact.
    let engine2 = RolloutDrillEngine::default();
    let stored = engine2.monitor_report_json(&base, &bad);
    reports.push(engine.run_recovery_drill("recovery_reproducible", &base, &bad, &stored));
    let tampered = stored.replace("hard_fail", "pass");
    reports.push(engine.run_recovery_drill(
        "recovery_detects_artifact_disagreement",
        &base,
        &bad,
        &tampered,
    ));

    reports
}

impl RolloutDrillEngine {
    /// Convenience: a raw monitor report JSON for storage-then-replay flows.
    #[must_use]
    pub fn monitor_report_json(&self, baseline: &MetricSeries, candidate: &MetricSeries) -> String {
        self.monitor.evaluate(baseline, candidate).to_json()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_clean_advances_and_regression_halts() {
        let engine = RolloutDrillEngine::default();
        let base = series(healthy());
        let clean = engine.run_shadow_drill("t", &base, &series(healthy()));
        assert!(clean.mechanism_ok);
        assert!(
            clean
                .steps
                .iter()
                .any(|s| s.observation == "advance_to_canary")
        );

        let regression = engine.run_shadow_drill("t", &base, &series(regressed()));
        assert!(
            regression
                .steps
                .iter()
                .any(|s| s.observation == "halt_before_exposure")
        );
        assert!(
            regression
                .operator_guidance
                .iter()
                .any(|g| g.contains("stop before any user exposure"))
        );
    }

    #[test]
    fn canary_aborts_at_the_regressing_stage_not_before() {
        let engine = RolloutDrillEngine::default();
        let base = series(healthy());
        let report = engine.run_canary_drill(
            "t",
            &base,
            &[
                ("10pct", series(healthy())),
                ("50pct", series(regressed())),
                ("100pct", series(healthy())),
            ],
        );
        assert!(report.mechanism_ok);
        assert!(
            report
                .steps
                .iter()
                .any(|s| s.observation == "aborted_at_stage_50pct")
        );
        // Stage 3 must never have been evaluated after the abort.
        assert!(
            !report
                .steps
                .iter()
                .any(|s| s.action == "canary_stage_100pct")
        );
    }

    #[test]
    fn canary_holds_on_a_warning_and_stops_widening() {
        let engine = RolloutDrillEngine::default();
        let base = series(healthy());
        // A warning-band candidate: p95/p99 land ~+12% over baseline (past
        // the +10% warning margin, under the +25% hard gate), everything
        // else within envelope.
        let warn_stage: Vec<u64> = (0..60)
            .map(|i| if i >= 55 { 122 } else { 100 + (i % 10) })
            .collect();
        let report = engine.run_canary_drill(
            "t",
            &base,
            &[
                ("10pct", series(healthy())),
                ("50pct", series(warn_stage)),
                ("100pct", series(healthy())),
            ],
        );
        assert!(report.mechanism_ok);
        assert!(
            report
                .steps
                .iter()
                .any(|s| s.observation == "held_at_stage_50pct"),
            "a warning must hold exposure at the warning stage"
        );
        // Widening must stop: the 100pct stage is never evaluated.
        assert!(
            !report
                .steps
                .iter()
                .any(|s| s.action == "canary_stage_100pct"),
            "exposure must not widen past a stage that needs review"
        );
        assert!(
            report
                .operator_guidance
                .iter()
                .any(|g| g.contains("held at stage 50pct"))
        );
    }

    #[test]
    fn fallback_mechanism_requires_real_trip_and_reports_restoration() {
        let engine = RolloutDrillEngine::default();
        let base = series(healthy());
        let report =
            engine.run_fallback_drill("t", &base, &series(regressed()), &series(healthy()));
        assert!(report.mechanism_ok, "trip must be detected");
        assert!(
            report
                .operator_guidance
                .iter()
                .any(|g| g.contains("restored the trusted envelope"))
        );

        // Failure path: fallback does NOT restore the envelope → escalation.
        let stuck =
            engine.run_fallback_drill("t", &base, &series(regressed()), &series(regressed()));
        assert!(
            stuck
                .operator_guidance
                .iter()
                .any(|g| g.contains("escalate to incident response"))
        );
    }

    #[test]
    fn rollback_demotes_and_restores() {
        let engine = RolloutDrillEngine::default();
        let report = engine.run_rollback_drill(
            "t",
            &evidence("c", true),
            &evidence("c", false),
            &series(healthy()),
            &series(healthy()),
        );
        assert!(report.mechanism_ok);
        assert!(
            report
                .steps
                .iter()
                .any(|s| s.observation.contains("rollback required"))
        );
    }

    #[test]
    fn recovery_trusts_reproducible_artifacts_and_quarantines_divergent_ones() {
        let engine = RolloutDrillEngine::default();
        let base = series(healthy());
        let bad = series(regressed());
        let stored = engine.monitor_report_json(&base, &bad);

        let ok = engine.run_recovery_drill("t", &base, &bad, &stored);
        assert!(
            ok.steps
                .iter()
                .any(|s| s.observation.contains("byte-identical"))
        );

        let tampered = stored.replace("hard_fail", "pass");
        let quarantined = engine.run_recovery_drill("t", &base, &bad, &tampered);
        assert!(
            quarantined
                .steps
                .iter()
                .any(|s| s.observation.contains("quarantined"))
        );
        assert!(
            quarantined
                .operator_guidance
                .iter()
                .any(|g| g.contains("replayed evaluation from raw samples wins"))
        );
    }

    #[test]
    fn standard_suite_covers_all_drills_with_clean_and_failure_paths() {
        let reports = standard_drill_suite();
        assert_eq!(reports.len(), 8);
        for kind in [
            DrillKind::Shadow,
            DrillKind::Canary,
            DrillKind::Fallback,
            DrillKind::Rollback,
            DrillKind::Recovery,
        ] {
            assert!(
                reports.iter().any(|r| r.kind == kind),
                "missing drill {kind:?}"
            );
        }
        for report in &reports {
            assert!(report.mechanism_ok, "drill machinery failed: {report:?}");
            assert!(!report.operator_guidance.is_empty());
        }
    }

    #[test]
    fn drill_reports_are_deterministic_and_parseable() {
        let a: Vec<String> = standard_drill_suite()
            .iter()
            .map(DrillReport::to_json)
            .collect();
        let b: Vec<String> = standard_drill_suite()
            .iter()
            .map(DrillReport::to_json)
            .collect();
        assert_eq!(a, b, "drill suite must replay byte-identically");
        for json in &a {
            let parsed: serde_json::Value = serde_json::from_str(json).expect("drill JSON parses");
            assert_eq!(
                parsed["schema_version"].as_str(),
                Some(PERF_ROLLOUT_DRILLS_SCHEMA_VERSION)
            );
            assert!(
                parsed["risk_controlled"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty())
            );
            assert!(parsed["steps"].as_array().is_some_and(|s| !s.is_empty()));
        }
    }
}
