#![forbid(unsafe_code)]

//! Rollout runbook: executable documentation for the Asupersync migration (bd-132iq).
//!
//! This module serves as both the operator/contributor runbook and a test suite.
//! Each section is a documented test function that exercises the actual rollout
//! APIs, ensuring the runbook never drifts from the implementation.
//!
//! # Migration overview
//!
//! FrankenTUI is migrating its runtime execution substrate from thread-based
//! subscriptions (`Legacy` lane) through structured cancellation (`Structured`
//! lane, the current default) toward Asupersync-native execution (`Asupersync`
//! lane, future).
//!
//! # Key concepts
//!
//! - **`RuntimeLane`**: Which execution backend is active (`Legacy`, `Structured`, `Asupersync`).
//! - **`RolloutPolicy`**: How the transition is managed (`Off`, `Shadow`, `Enabled`).
//! - **`RolloutScorecard`**: Combines shadow-run + benchmark evidence into a Go/NoGo verdict.
//! - **`RolloutEvidenceBundle`**: Self-contained JSON artifact for release decisions.
//! - **`MigrationReadinessRubric`**: Alpha/beta/GA gate policy for OpenTUI migrations.
//!
//! # Anti-goals
//!
//! - Do NOT async-ify the pure rendering kernel (buffer, diff, presenter).
//! - Do NOT break any existing public APIs.
//! - Do NOT enable the Asupersync lane without shadow evidence proving determinism.
//!
//! # Operator workflow
//!
//! 1. **Baseline**: Run with default config (`RolloutPolicy::Off`, `RuntimeLane::Structured`).
//! 2. **Shadow**: Set `FTUI_ROLLOUT_POLICY=shadow` to gather comparison evidence.
//! 3. **Evaluate**: Feed shadow results into `RolloutScorecard` → check verdict.
//! 4. **Stage gate**: Evaluate certification, corpus coverage, reliability, deterministic artifacts, blockers, and operator authority with `MigrationReadinessRubric`.
//! 5. **Enable**: If Go and the target stage gate advances, set `FTUI_ROLLOUT_POLICY=enabled` + `FTUI_RUNTIME_LANE=asupersync`.
//! 6. **Monitor**: Watch queue telemetry (`effects_queue_dropped`, `effects_queue_high_water`).
//! 7. **Rollback or hold**: If problems, set `FTUI_ROLLOUT_POLICY=off` + `FTUI_RUNTIME_LANE=structured`; any active emergency hold blocks stage advancement.
//!
//! # Environment variables
//!
//! | Variable | Values | Default | Purpose |
//! |----------|--------|---------|---------|
//! | `FTUI_RUNTIME_LANE` | `legacy`, `structured`, `asupersync` | `structured` | Select execution backend |
//! | `FTUI_ROLLOUT_POLICY` | `off`, `shadow`, `enabled` | `off` | Control rollout behavior |
//!
//! # Scripts and evidence
//!
//! - `scripts/perf_regression_gate.sh` — CI performance gate
//! - `tests/baseline.json` — Performance baselines (render + runtime)
//! - `RolloutEvidenceBundle::to_json()` — Machine-readable release decision artifact
//! - `MigrationReadinessDecision::to_json()` — Stage gate decision artifact

#[cfg(test)]
mod tests {
    use ftui_runtime::program::{ProgramConfig, RolloutPolicy, RuntimeLane};

    // =================================================================
    // Step 1: Baseline configuration (default production state)
    // =================================================================

    /// Verify the default configuration matches the expected baseline.
    ///
    /// Operators start here. The default is `Structured` lane with rollout
    /// policy `Off` — identical to pre-migration behavior.
    #[test]
    fn runbook_step1_baseline_defaults() {
        let config = ProgramConfig::default();

        assert_eq!(
            config.runtime_lane,
            RuntimeLane::Structured,
            "Default lane must be Structured (current migration state)"
        );
        assert_eq!(
            config.rollout_policy,
            RolloutPolicy::Off,
            "Default rollout policy must be Off (no shadow comparison)"
        );
    }

    // =================================================================
    // Step 2: Enable shadow mode for evidence gathering
    // =================================================================

    /// Verify shadow mode can be activated via builder.
    ///
    /// Operators set `FTUI_ROLLOUT_POLICY=shadow` to enable shadow
    /// comparison. The lane stays the same — shadow mode only adds
    /// comparison, it doesn't change which lane renders.
    #[test]
    fn runbook_step2_enable_shadow_mode() {
        let config = ProgramConfig::default().with_rollout_policy(RolloutPolicy::Shadow);

        assert_eq!(config.rollout_policy, RolloutPolicy::Shadow);
        assert!(config.rollout_policy.is_shadow());
        // Lane is unchanged — shadow mode doesn't affect rendering
        assert_eq!(config.runtime_lane, RuntimeLane::Structured);
    }

    // =================================================================
    // Step 3: Evaluate shadow evidence
    // =================================================================

    /// Verify the scorecard correctly evaluates shadow evidence.
    ///
    /// After gathering shadow data, operators feed results into
    /// `RolloutScorecard` to get a Go/NoGo/Inconclusive verdict.
    #[test]
    fn runbook_step3_evaluate_scorecard() {
        use crate::rollout_scorecard::{RolloutScorecard, RolloutScorecardConfig, RolloutVerdict};
        use crate::shadow_run::{ShadowRun, ShadowRunConfig};

        use ftui_core::event::Event;
        use ftui_render::frame::Frame;
        use ftui_runtime::program::{Cmd, Model};
        use ftui_widgets::Widget;
        use ftui_widgets::paragraph::Paragraph;

        // Minimal model for runbook demonstration
        struct RunbookModel {
            count: u32,
        }
        #[derive(Debug, Clone)]
        enum Msg {
            Tick,
        }
        impl From<Event> for Msg {
            fn from(_: Event) -> Self {
                Msg::Tick
            }
        }
        impl Model for RunbookModel {
            type Message = Msg;
            fn update(&mut self, _msg: Msg) -> Cmd<Msg> {
                self.count += 1;
                Cmd::none()
            }
            fn view(&self, frame: &mut Frame) {
                let text = format!("{}", self.count);
                let area = ftui_core::geometry::Rect::new(0, 0, frame.width(), 1);
                Paragraph::new(text).render(area, frame);
            }
        }

        // Run shadow comparison
        let shadow_config = ShadowRunConfig::new("runbook", "step3", 42).viewport(40, 10);
        let result = ShadowRun::compare(
            shadow_config,
            || RunbookModel { count: 0 },
            |session: &mut crate::lab_integration::LabSession<RunbookModel>| {
                session.init();
                session.tick();
                session.capture_frame();
            },
        );

        // Feed into scorecard
        let mut scorecard =
            RolloutScorecard::new(RolloutScorecardConfig::default().min_shadow_scenarios(1));
        scorecard.add_shadow_result(result);

        let verdict = scorecard.evaluate();
        assert_eq!(
            verdict,
            RolloutVerdict::Go,
            "Identical models must produce Go verdict"
        );
    }

    // =================================================================
    // Step 4: Promote to enabled (with Asupersync lane)
    // =================================================================

    /// Verify the enable transition and fallback behavior.
    ///
    /// When shadow evidence is good, operators promote to Enabled.
    /// Note: Asupersync lane currently resolves to Structured (fallback)
    /// until the Asupersync executor is fully implemented.
    #[test]
    fn runbook_step4_promote_to_enabled() {
        let config = ProgramConfig::default()
            .with_lane(RuntimeLane::Asupersync)
            .with_rollout_policy(RolloutPolicy::Enabled);

        assert_eq!(config.rollout_policy, RolloutPolicy::Enabled);
        assert_eq!(config.runtime_lane, RuntimeLane::Asupersync);

        // Asupersync resolves to Structured until fully implemented
        let resolved = config.runtime_lane.resolve();
        assert_eq!(
            resolved,
            RuntimeLane::Structured,
            "Asupersync must fall back to Structured until fully implemented"
        );
    }

    // =================================================================
    // Step 5: Monitor queue telemetry
    // =================================================================

    /// Verify queue telemetry is accessible for monitoring.
    ///
    /// Operators watch `effects_queue_dropped` and `effects_queue_high_water`
    /// for signs of trouble after enabling the new lane.
    #[test]
    fn runbook_step5_monitor_queue_telemetry() {
        let snap = ftui_runtime::effect_system::queue_telemetry();

        // Telemetry snapshot must be internally consistent
        assert_eq!(
            snap.in_flight,
            snap.enqueued
                .saturating_sub(snap.processed)
                .saturating_sub(snap.dropped),
            "in_flight must equal enqueued - processed - dropped"
        );
    }

    // =================================================================
    // Step 6: Rollback procedure
    // =================================================================

    /// Verify the rollback path works correctly.
    ///
    /// If problems are detected, operators roll back by setting both
    /// the lane and policy back to safe defaults.
    #[test]
    fn runbook_step6_rollback() {
        // Start in enabled state
        let config = ProgramConfig::default()
            .with_lane(RuntimeLane::Asupersync)
            .with_rollout_policy(RolloutPolicy::Enabled);

        // Rollback: return to safe defaults
        let config = config
            .with_lane(RuntimeLane::Structured)
            .with_rollout_policy(RolloutPolicy::Off);

        assert_eq!(config.runtime_lane, RuntimeLane::Structured);
        assert_eq!(config.rollout_policy, RolloutPolicy::Off);
    }

    // =================================================================
    // Step 7: Generate evidence bundle for release decision
    // =================================================================

    /// Verify the evidence bundle produces valid JSON for CI/dashboards.
    ///
    /// The `RolloutEvidenceBundle` is the single artifact that captures
    /// everything needed for a go/no-go release decision.
    #[test]
    fn runbook_step7_evidence_bundle() {
        use crate::rollout_scorecard::{
            RolloutEvidenceBundle, RolloutScorecard, RolloutScorecardConfig,
        };

        let scorecard =
            RolloutScorecard::new(RolloutScorecardConfig::default().min_shadow_scenarios(0));
        let summary = scorecard.summary();

        let bundle = RolloutEvidenceBundle {
            scorecard: summary,
            queue_telemetry: Some(ftui_runtime::effect_system::queue_telemetry()),
            requested_lane: RuntimeLane::Structured.label().to_string(),
            resolved_lane: RuntimeLane::Structured.label().to_string(),
            rollout_policy: RolloutPolicy::Off.label().to_string(),
        };

        let json = bundle.to_json();
        assert!(json.starts_with('{'), "evidence must be valid JSON object");
        assert!(json.ends_with('}'), "evidence must be valid JSON object");
        assert!(
            json.contains("\"schema_version\""),
            "evidence must include schema version"
        );
        assert!(
            json.contains("\"scorecard\""),
            "evidence must include scorecard"
        );
        assert!(
            json.contains("\"runtime\""),
            "evidence must include runtime info"
        );
    }

    // =================================================================
    // Step 8: Evaluate OpenTUI migration readiness stage
    // =================================================================

    /// Verify the alpha/beta/GA readiness rubric is executable.
    ///
    /// Operators must attach deterministic artifact evidence before advancing
    /// a migration stage. Emergency holds override otherwise passing metrics.
    #[test]
    fn runbook_step8_migration_readiness_rubric() {
        use crate::rollout_scorecard::{
            EmergencyHold, EmergencyHoldReason, MigrationReadinessEvidence,
            MigrationReadinessRubric, MigrationReadinessVerdict, MigrationRolloutStage,
            OperatorAuthority,
        };

        let rubric = MigrationReadinessRubric::opentui_default();
        let beta_ready = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.98)
            .corpus_coverage_ratio(0.70)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(5)
            .benchmark_gate_passed(true);

        let decision = rubric.evaluate(MigrationRolloutStage::Beta, &beta_ready);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::Advance);
        assert_eq!(
            decision.to_json(),
            "{\"stage\":\"beta\",\"verdict\":\"advance\",\"reasons\":[]}"
        );

        let held = beta_ready.emergency_hold(EmergencyHold::new(
            EmergencyHoldReason::OperatorOverride,
            OperatorAuthority::OnCall,
        ));
        let decision = rubric.evaluate(MigrationRolloutStage::Beta, &held);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::EmergencyHold);
        assert!(
            decision.reasons.contains(&"operator-override"),
            "operator emergency hold must block advancement"
        );
    }

    // =================================================================
    // Environment variable parsing
    // =================================================================

    /// Verify all documented environment variable values parse correctly.
    ///
    /// This test ensures the runbook's env var table stays accurate.
    #[test]
    fn runbook_env_var_parsing() {
        // RuntimeLane values from the runbook table
        assert_eq!(RuntimeLane::parse("legacy"), Some(RuntimeLane::Legacy));
        assert_eq!(
            RuntimeLane::parse("structured"),
            Some(RuntimeLane::Structured)
        );
        assert_eq!(
            RuntimeLane::parse("asupersync"),
            Some(RuntimeLane::Asupersync)
        );

        // RolloutPolicy values from the runbook table
        assert_eq!(RolloutPolicy::parse("off"), Some(RolloutPolicy::Off));
        assert_eq!(RolloutPolicy::parse("shadow"), Some(RolloutPolicy::Shadow));
        assert_eq!(
            RolloutPolicy::parse("enabled"),
            Some(RolloutPolicy::Enabled)
        );
    }
}
