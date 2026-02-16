//! E2E integration tests for the DecisionCore<S,A> expected-loss framework.
//!
//! Exercises all 4 states (stable/bursty/resize/degraded) through the unified
//! decision framework, verifying optimal action selection, state transitions,
//! calibration accuracy, and JSONL evidence logging.

use ftui_runtime::decision_core::{
    Action, Decision, DecisionCore, Outcome, Posterior, State, argmin_expected_loss,
    second_best_loss,
};
use ftui_runtime::unified_evidence::{DecisionDomain, EvidenceTerm, UnifiedEvidenceLedger};

// ============================================================================
// Domain Types
// ============================================================================

/// Regime state for the rendering pipeline.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Regime {
    Stable,
    Bursty,
    Resize,
    Degraded,
}

impl State for Regime {}
impl Outcome for Regime {}

/// Posterior belief: probability distribution over regimes.
#[derive(Debug, Clone)]
struct RegimePosterior {
    stable: f64,
    bursty: f64,
    resize: f64,
    degraded: f64,
}

impl RegimePosterior {
    fn new(stable: f64, bursty: f64, resize: f64, degraded: f64) -> Self {
        Self {
            stable,
            bursty,
            resize,
            degraded,
        }
    }

    fn normalize(&mut self) {
        let sum = self.stable + self.bursty + self.resize + self.degraded;
        if sum > 0.0 {
            self.stable /= sum;
            self.bursty /= sum;
            self.resize /= sum;
            self.degraded /= sum;
        }
    }

    fn sums_to_one(&self) -> bool {
        let sum = self.stable + self.bursty + self.resize + self.degraded;
        (sum - 1.0).abs() < 1e-6
    }

    fn dominant(&self) -> Regime {
        let probs = [
            (self.stable, Regime::Stable),
            (self.bursty, Regime::Bursty),
            (self.resize, Regime::Resize),
            (self.degraded, Regime::Degraded),
        ];
        probs
            .iter()
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
            .unwrap()
            .1
    }
}

/// Available rendering actions.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RenderAction {
    IncrementalDiff,
    FullDiff,
    Deferred,
}

impl Action for RenderAction {
    fn label(&self) -> &'static str {
        match self {
            Self::IncrementalDiff => "incremental_diff",
            Self::FullDiff => "full_diff",
            Self::Deferred => "deferred",
        }
    }
}

// ============================================================================
// Loss Matrix
// ============================================================================

/// 4×3 loss matrix: L[state][action].
///
/// Lower = better for that (state, action) pair.
///
/// ```text
///                 incr_diff  full_diff  deferred
/// stable            1.0        3.0       5.0
/// bursty            4.0        2.0       3.5
/// resize            6.0        5.0       1.0
/// degraded          8.0        2.5       4.0
/// ```
const LOSS_MATRIX: [[f64; 3]; 4] = [
    [1.0, 3.0, 5.0], // stable: incremental is best
    [4.0, 2.0, 3.5], // bursty: full_diff is best
    [6.0, 5.0, 1.0], // resize: deferred is best
    [8.0, 2.5, 4.0], // degraded: full_diff is best
];

fn regime_index(r: Regime) -> usize {
    match r {
        Regime::Stable => 0,
        Regime::Bursty => 1,
        Regime::Resize => 2,
        Regime::Degraded => 3,
    }
}

fn action_index(a: RenderAction) -> usize {
    match a {
        RenderAction::IncrementalDiff => 0,
        RenderAction::FullDiff => 1,
        RenderAction::Deferred => 2,
    }
}

// ============================================================================
// Controller
// ============================================================================

/// Full-featured rendering decision controller.
///
/// Uses a categorical posterior over 4 regimes and a fixed loss matrix.
/// Calibration uses exponential moving average toward the observed regime.
struct RenderController {
    posterior: RegimePosterior,
    /// EMA smoothing factor (0 < alpha <= 1).
    alpha: f64,
    /// Running count of decisions made.
    decision_count: u64,
    /// Running count of calibrations received.
    calibration_count: u64,
}

impl RenderController {
    fn new(initial: RegimePosterior) -> Self {
        let mut p = initial;
        p.normalize();
        Self {
            posterior: p,
            alpha: 0.3,
            decision_count: 0,
            calibration_count: 0,
        }
    }

    /// Compute expected loss for a given action under the current posterior.
    fn expected_loss_for(&self, action: RenderAction) -> f64 {
        let ai = action_index(action);
        self.posterior.stable * LOSS_MATRIX[0][ai]
            + self.posterior.bursty * LOSS_MATRIX[1][ai]
            + self.posterior.resize * LOSS_MATRIX[2][ai]
            + self.posterior.degraded * LOSS_MATRIX[3][ai]
    }
}

impl DecisionCore<Regime, RenderAction> for RenderController {
    type Outcome = Regime;

    fn domain(&self) -> DecisionDomain {
        DecisionDomain::DiffStrategy
    }

    fn posterior(&self, _evidence: &[EvidenceTerm]) -> Posterior<Regime> {
        let dominant = self.posterior.dominant();
        let dominant_prob = match dominant {
            Regime::Stable => self.posterior.stable,
            Regime::Bursty => self.posterior.bursty,
            Regime::Resize => self.posterior.resize,
            Regime::Degraded => self.posterior.degraded,
        };
        let odds = dominant_prob / (1.0 - dominant_prob).max(1e-10);
        Posterior {
            point_estimate: dominant,
            log_posterior: odds.ln(),
            confidence_interval: (dominant_prob - 0.05, dominant_prob + 0.05),
            evidence: Vec::new(),
        }
    }

    fn loss(&self, action: &RenderAction, state: &Regime) -> f64 {
        LOSS_MATRIX[regime_index(*state)][action_index(*action)]
    }

    fn decide(&mut self, evidence: &[EvidenceTerm]) -> Decision<RenderAction> {
        let posterior = self.posterior(evidence);
        let actions = self.actions();

        // Full expected-loss computation over the posterior distribution.
        let mut losses: Vec<(usize, f64)> = actions
            .iter()
            .enumerate()
            .map(|(i, a)| (i, self.expected_loss_for(*a)))
            .collect();
        losses.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        let (best_idx, best_loss) = losses[0];
        let next_best_loss = if losses.len() > 1 {
            losses[1].1
        } else {
            best_loss
        };

        self.decision_count += 1;

        Decision {
            action: actions[best_idx],
            expected_loss: best_loss,
            next_best_loss,
            log_posterior: posterior.log_posterior,
            confidence_interval: posterior.confidence_interval,
            evidence: posterior.evidence,
        }
    }

    fn calibrate(&mut self, outcome: &Regime) {
        // EMA update: push probability mass toward the observed regime.
        let target = match outcome {
            Regime::Stable => RegimePosterior::new(1.0, 0.0, 0.0, 0.0),
            Regime::Bursty => RegimePosterior::new(0.0, 1.0, 0.0, 0.0),
            Regime::Resize => RegimePosterior::new(0.0, 0.0, 1.0, 0.0),
            Regime::Degraded => RegimePosterior::new(0.0, 0.0, 0.0, 1.0),
        };
        let a = self.alpha;
        self.posterior.stable = (1.0 - a) * self.posterior.stable + a * target.stable;
        self.posterior.bursty = (1.0 - a) * self.posterior.bursty + a * target.bursty;
        self.posterior.resize = (1.0 - a) * self.posterior.resize + a * target.resize;
        self.posterior.degraded = (1.0 - a) * self.posterior.degraded + a * target.degraded;
        self.posterior.normalize();
        self.calibration_count += 1;
    }

    fn fallback_action(&self) -> RenderAction {
        RenderAction::FullDiff
    }

    fn actions(&self) -> Vec<RenderAction> {
        vec![
            RenderAction::IncrementalDiff,
            RenderAction::FullDiff,
            RenderAction::Deferred,
        ]
    }
}

// ============================================================================
// JSONL Log Entry (for structured evidence)
// ============================================================================

/// Structured decision log for JSONL output.
#[derive(Debug)]
struct DecisionLog {
    frame_id: u64,
    current_state: Regime,
    posterior: RegimePosterior,
    action_chosen: RenderAction,
    expected_losses: [f64; 3],
    is_optimal: bool,
}

impl DecisionLog {
    fn to_json(&self) -> String {
        let state_str = match self.current_state {
            Regime::Stable => "stable",
            Regime::Bursty => "bursty",
            Regime::Resize => "resize",
            Regime::Degraded => "degraded",
        };
        format!(
            r#"{{"event":"expected_loss_decision","frame_id":{},"current_state":"{}","state_posteriors":{{"stable":{:.6},"bursty":{:.6},"resize":{:.6},"degraded":{:.6}}},"action_chosen":"{}","expected_losses":{{"incremental_diff":{:.6},"full_diff":{:.6},"deferred":{:.6}}},"is_optimal":{}}}"#,
            self.frame_id,
            state_str,
            self.posterior.stable,
            self.posterior.bursty,
            self.posterior.resize,
            self.posterior.degraded,
            self.action_chosen.label(),
            self.expected_losses[0],
            self.expected_losses[1],
            self.expected_losses[2],
            self.is_optimal,
        )
    }
}

// ============================================================================
// Helper: run a sequence of frames with a given regime
// ============================================================================

fn run_frames(
    ctrl: &mut RenderController,
    ledger: &mut UnifiedEvidenceLedger,
    regime: Regime,
    count: u64,
    start_frame: u64,
    logs: &mut Vec<DecisionLog>,
) {
    for i in 0..count {
        let frame_id = start_frame + i;
        let ts_ns = frame_id * 16_667_000; // ~60fps

        let decision = ctrl.decide_and_record(&[], ledger, ts_ns);

        let expected_losses = [
            ctrl.expected_loss_for(RenderAction::IncrementalDiff),
            ctrl.expected_loss_for(RenderAction::FullDiff),
            ctrl.expected_loss_for(RenderAction::Deferred),
        ];

        // Check optimality: chosen action must have minimum expected loss.
        let min_loss = expected_losses
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let is_optimal = (decision.expected_loss - min_loss).abs() < 1e-10;

        logs.push(DecisionLog {
            frame_id,
            current_state: regime,
            posterior: ctrl.posterior.clone(),
            action_chosen: decision.action,
            expected_losses,
            is_optimal,
        });

        // Calibrate with the true regime.
        ctrl.calibrate(&regime);
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Steady-state: stable regime → incremental_diff is optimal.
#[test]
fn stable_regime_chooses_incremental() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.7, 0.1, 0.1, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 20, 0, &mut logs);

    // After calibrating on stable, incremental should dominate.
    let last = logs.last().unwrap();
    assert_eq!(last.action_chosen, RenderAction::IncrementalDiff);
    assert!(last.is_optimal);
    assert!(ctrl.posterior.stable > 0.8);
}

/// Bursty regime → full_diff is optimal.
#[test]
fn bursty_regime_chooses_full_diff() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.1, 0.7, 0.1, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Bursty, 20, 0, &mut logs);

    let last = logs.last().unwrap();
    assert_eq!(last.action_chosen, RenderAction::FullDiff);
    assert!(last.is_optimal);
    assert!(ctrl.posterior.bursty > 0.8);
}

/// Resize regime → deferred is optimal.
#[test]
fn resize_regime_chooses_deferred() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.1, 0.1, 0.7, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Resize, 20, 0, &mut logs);

    let last = logs.last().unwrap();
    assert_eq!(last.action_chosen, RenderAction::Deferred);
    assert!(last.is_optimal);
    assert!(ctrl.posterior.resize > 0.8);
}

/// Degraded regime → full_diff is optimal.
#[test]
fn degraded_regime_chooses_full_diff() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.1, 0.1, 0.1, 0.7));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Degraded, 20, 0, &mut logs);

    let last = logs.last().unwrap();
    assert_eq!(last.action_chosen, RenderAction::FullDiff);
    assert!(last.is_optimal);
    assert!(ctrl.posterior.degraded > 0.8);
}

/// Transition: stable → bursty. Controller adapts action.
#[test]
fn transition_stable_to_bursty() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.85, 0.05, 0.05, 0.05));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();

    // Phase 1: stable for 10 frames.
    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 10, 0, &mut logs);
    assert_eq!(
        logs.last().unwrap().action_chosen,
        RenderAction::IncrementalDiff
    );

    // Phase 2: switch to bursty for 20 frames.
    run_frames(&mut ctrl, &mut ledger, Regime::Bursty, 20, 10, &mut logs);
    assert_eq!(logs.last().unwrap().action_chosen, RenderAction::FullDiff);
    assert!(ctrl.posterior.bursty > ctrl.posterior.stable);
}

/// Transition: bursty → resize. Controller adapts action.
#[test]
fn transition_bursty_to_resize() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.05, 0.85, 0.05, 0.05));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Bursty, 10, 0, &mut logs);
    assert_eq!(logs.last().unwrap().action_chosen, RenderAction::FullDiff);

    run_frames(&mut ctrl, &mut ledger, Regime::Resize, 20, 10, &mut logs);
    assert_eq!(logs.last().unwrap().action_chosen, RenderAction::Deferred);
    assert!(ctrl.posterior.resize > ctrl.posterior.bursty);
}

/// Transition: resize → stable. Controller recovers.
#[test]
fn transition_resize_to_stable() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.05, 0.05, 0.85, 0.05));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Resize, 10, 0, &mut logs);
    assert_eq!(logs.last().unwrap().action_chosen, RenderAction::Deferred);

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 20, 10, &mut logs);
    assert_eq!(
        logs.last().unwrap().action_chosen,
        RenderAction::IncrementalDiff
    );
}

/// Transition: stable → degraded. Controller shifts to conservative action.
#[test]
fn transition_stable_to_degraded() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.85, 0.05, 0.05, 0.05));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 10, 0, &mut logs);
    assert_eq!(
        logs.last().unwrap().action_chosen,
        RenderAction::IncrementalDiff
    );

    run_frames(&mut ctrl, &mut ledger, Regime::Degraded, 20, 10, &mut logs);
    assert_eq!(logs.last().unwrap().action_chosen, RenderAction::FullDiff);
}

/// Transition: degraded → stable. Controller recovers.
#[test]
fn transition_degraded_to_stable() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.05, 0.05, 0.05, 0.85));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Degraded, 10, 0, &mut logs);
    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 20, 10, &mut logs);
    assert_eq!(
        logs.last().unwrap().action_chosen,
        RenderAction::IncrementalDiff
    );
}

/// Full 5-transition cycle through all regimes.
#[test]
fn full_five_transition_cycle() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.85, 0.05, 0.05, 0.05));
    let mut ledger = UnifiedEvidenceLedger::new(1000);
    let mut logs = Vec::new();
    let mut frame = 0u64;

    // stable → bursty → resize → stable → degraded → stable
    let phases: &[(Regime, u64)] = &[
        (Regime::Stable, 15),
        (Regime::Bursty, 15),
        (Regime::Resize, 15),
        (Regime::Stable, 15),
        (Regime::Degraded, 15),
        (Regime::Stable, 15),
    ];

    for &(regime, count) in phases {
        run_frames(&mut ctrl, &mut ledger, regime, count, frame, &mut logs);
        frame += count;
    }

    assert_eq!(logs.len(), 90);
    assert_eq!(ledger.len(), 90);

    // Final state should be stable.
    assert_eq!(
        logs.last().unwrap().action_chosen,
        RenderAction::IncrementalDiff
    );
    assert!(ctrl.posterior.stable > 0.7);
}

/// Every decision is optimal: chosen action minimizes expected loss.
#[test]
fn all_decisions_are_optimal() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    let mut ledger = UnifiedEvidenceLedger::new(1000);
    let mut logs = Vec::new();
    let mut frame = 0u64;

    let phases: &[(Regime, u64)] = &[
        (Regime::Stable, 20),
        (Regime::Bursty, 20),
        (Regime::Resize, 20),
        (Regime::Degraded, 20),
    ];

    for &(regime, count) in phases {
        run_frames(&mut ctrl, &mut ledger, regime, count, frame, &mut logs);
        frame += count;
    }

    // Every single decision must be optimal.
    for log in &logs {
        assert!(
            log.is_optimal,
            "Non-optimal decision at frame {}: action={:?}, losses={:?}",
            log.frame_id, log.action_chosen, log.expected_losses,
        );
    }
}

/// Posterior probabilities sum to 1.0 at every decision point.
#[test]
fn posteriors_sum_to_one() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.4, 0.3, 0.2, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 10, 0, &mut logs);
    run_frames(&mut ctrl, &mut ledger, Regime::Bursty, 10, 10, &mut logs);

    for log in &logs {
        assert!(
            log.posterior.sums_to_one(),
            "Posterior does not sum to 1.0 at frame {}: s={:.6} b={:.6} r={:.6} d={:.6}",
            log.frame_id,
            log.posterior.stable,
            log.posterior.bursty,
            log.posterior.resize,
            log.posterior.degraded,
        );
    }
}

/// Calibration convergence: after 50 frames of a regime, posterior > 0.9.
#[test]
fn calibration_converges() {
    for regime in &[
        Regime::Stable,
        Regime::Bursty,
        Regime::Resize,
        Regime::Degraded,
    ] {
        let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
        let mut ledger = UnifiedEvidenceLedger::new(200);
        let mut logs = Vec::new();

        run_frames(&mut ctrl, &mut ledger, *regime, 50, 0, &mut logs);

        let prob = match regime {
            Regime::Stable => ctrl.posterior.stable,
            Regime::Bursty => ctrl.posterior.bursty,
            Regime::Resize => ctrl.posterior.resize,
            Regime::Degraded => ctrl.posterior.degraded,
        };
        assert!(
            prob > 0.9,
            "Calibration did not converge for {:?}: prob={:.6}",
            regime,
            prob,
        );
    }
}

/// Ledger records correct domain for all entries.
#[test]
fn ledger_domain_is_diff_strategy() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.85, 0.05, 0.05, 0.05));
    let mut ledger = UnifiedEvidenceLedger::new(100);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 30, 0, &mut logs);

    assert_eq!(ledger.len(), 30);
    for entry in ledger.entries() {
        assert_eq!(entry.domain, DecisionDomain::DiffStrategy);
    }
}

/// Ledger decision IDs are monotonically increasing.
#[test]
fn ledger_ids_monotonic() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.7, 0.1, 0.1, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 50, 0, &mut logs);

    let ids: Vec<u64> = ledger.entries().map(|e| e.decision_id).collect();
    for w in ids.windows(2) {
        assert!(w[0] < w[1], "Non-monotonic IDs: {} >= {}", w[0], w[1]);
    }
}

/// Loss_avoided is always non-negative.
#[test]
fn loss_avoided_nonnegative() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();
    let mut frame = 0u64;

    for &(regime, count) in &[
        (Regime::Stable, 15),
        (Regime::Bursty, 15),
        (Regime::Resize, 15),
        (Regime::Degraded, 15),
    ] {
        run_frames(&mut ctrl, &mut ledger, regime, count, frame, &mut logs);
        frame += count;
    }

    for entry in ledger.entries() {
        assert!(
            entry.loss_avoided >= 0.0,
            "Negative loss_avoided: {} at decision {}",
            entry.loss_avoided,
            entry.decision_id,
        );
    }
}

/// JSONL output is valid JSON for every decision log entry.
#[test]
fn jsonl_logs_are_valid_json() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();
    let mut frame = 0u64;

    let phases: &[(Regime, u64)] = &[
        (Regime::Stable, 10),
        (Regime::Bursty, 10),
        (Regime::Resize, 10),
        (Regime::Degraded, 10),
    ];

    for &(regime, count) in phases {
        run_frames(&mut ctrl, &mut ledger, regime, count, frame, &mut logs);
        frame += count;
    }

    for log in &logs {
        let json_str = log.to_json();
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("JSONL line is not valid JSON");

        // Required fields.
        assert_eq!(parsed["event"], "expected_loss_decision");
        assert!(parsed["frame_id"].is_u64());
        assert!(parsed["current_state"].is_string());
        assert!(parsed["state_posteriors"]["stable"].is_f64());
        assert!(parsed["state_posteriors"]["bursty"].is_f64());
        assert!(parsed["state_posteriors"]["resize"].is_f64());
        assert!(parsed["state_posteriors"]["degraded"].is_f64());
        assert!(parsed["action_chosen"].is_string());
        assert!(parsed["expected_losses"]["incremental_diff"].is_f64());
        assert!(parsed["expected_losses"]["full_diff"].is_f64());
        assert!(parsed["expected_losses"]["deferred"].is_f64());
        assert!(parsed["is_optimal"].is_boolean());
    }
}

/// Ledger JSONL export lines are all valid schema-v2 entries.
#[test]
fn ledger_jsonl_export_valid() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.7, 0.1, 0.1, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 20, 0, &mut logs);

    let jsonl = ledger.export_jsonl();
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), 20);

    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("Ledger JSONL line is not valid JSON");
        assert_eq!(parsed["schema"], "ftui-evidence-v2");
        assert!(parsed["id"].is_u64());
        assert!(parsed["ts_ns"].is_u64());
        assert_eq!(parsed["domain"], "diff_strategy");
        assert!(parsed["log_posterior"].is_f64());
        assert!(parsed["action"].is_string());
    }
}

/// Fallback action is always full_diff (conservative).
#[test]
fn fallback_action_is_conservative() {
    let ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    assert_eq!(ctrl.fallback_action(), RenderAction::FullDiff);
}

/// argmin helper returns correct minimum over the loss matrix.
#[test]
fn argmin_helper_correct() {
    let actions = vec![
        RenderAction::IncrementalDiff,
        RenderAction::FullDiff,
        RenderAction::Deferred,
    ];

    // Stable: incremental (loss 1.0) < full (3.0) < deferred (5.0)
    let (idx, loss) = argmin_expected_loss(&actions, &Regime::Stable, |a, s| {
        LOSS_MATRIX[regime_index(*s)][action_index(*a)]
    })
    .unwrap();
    assert_eq!(idx, 0);
    assert!((loss - 1.0).abs() < 1e-10);

    // Resize: deferred (loss 1.0) < full (5.0) < incremental (6.0)
    let (idx, loss) = argmin_expected_loss(&actions, &Regime::Resize, |a, s| {
        LOSS_MATRIX[regime_index(*s)][action_index(*a)]
    })
    .unwrap();
    assert_eq!(idx, 2);
    assert!((loss - 1.0).abs() < 1e-10);
}

/// second_best_loss helper returns the runner-up.
#[test]
fn second_best_helper_correct() {
    let actions = vec![
        RenderAction::IncrementalDiff,
        RenderAction::FullDiff,
        RenderAction::Deferred,
    ];

    // Stable: best=incremental(1.0), second=full(3.0)
    let sb = second_best_loss(&actions, &Regime::Stable, 0, |a, s| {
        LOSS_MATRIX[regime_index(*s)][action_index(*a)]
    });
    assert!((sb - 3.0).abs() < 1e-10);
}

/// Determinism: identical controllers produce identical decisions.
#[test]
fn deterministic_decisions() {
    let init = RegimePosterior::new(0.4, 0.3, 0.2, 0.1);
    let mut ctrl_a = RenderController::new(init.clone());
    let mut ctrl_b = RenderController::new(init);

    for _ in 0..10 {
        let d_a = ctrl_a.decide(&[]);
        let d_b = ctrl_b.decide(&[]);
        assert_eq!(d_a.action, d_b.action);
        assert!((d_a.expected_loss - d_b.expected_loss).abs() < 1e-10);

        ctrl_a.calibrate(&Regime::Stable);
        ctrl_b.calibrate(&Regime::Stable);
    }
}

/// Large-scale simulation: 600 frames across all regimes.
#[test]
fn large_scale_simulation() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    let mut ledger = UnifiedEvidenceLedger::new(600);
    let mut logs = Vec::new();
    let mut frame = 0u64;

    let phases: &[(Regime, u64)] = &[
        (Regime::Stable, 150),
        (Regime::Bursty, 150),
        (Regime::Resize, 150),
        (Regime::Degraded, 150),
    ];

    for &(regime, count) in phases {
        run_frames(&mut ctrl, &mut ledger, regime, count, frame, &mut logs);
        frame += count;
    }

    assert_eq!(logs.len(), 600);
    assert_eq!(ledger.len(), 600);
    assert_eq!(ledger.total_recorded(), 600);

    // All decisions are optimal.
    let non_optimal: Vec<_> = logs.iter().filter(|l| !l.is_optimal).collect();
    assert!(
        non_optimal.is_empty(),
        "Found {} non-optimal decisions",
        non_optimal.len(),
    );

    // All posteriors sum to 1.
    for log in &logs {
        assert!(log.posterior.sums_to_one());
    }

    // Domain count matches.
    assert_eq!(ledger.domain_count(DecisionDomain::DiffStrategy), 600,);
}

/// Timestamp ordering in ledger entries.
#[test]
fn timestamps_monotonic_in_ledger() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.7, 0.1, 0.1, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 50, 0, &mut logs);

    let timestamps: Vec<u64> = ledger.entries().map(|e| e.timestamp_ns).collect();
    for w in timestamps.windows(2) {
        assert!(
            w[0] < w[1],
            "Non-monotonic timestamps: {} >= {}",
            w[0],
            w[1]
        );
    }
}

/// Confidence intervals are well-formed (lower <= upper).
#[test]
fn confidence_intervals_well_formed() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 30, 0, &mut logs);

    for entry in ledger.entries() {
        let (lo, hi) = entry.confidence_interval;
        assert!(
            lo <= hi,
            "CI not well-formed: ({}, {}) at decision {}",
            lo,
            hi,
            entry.decision_id,
        );
    }
}

/// Expected loss for chosen action <= all alternatives at each step.
#[test]
fn chosen_action_minimizes_expected_loss() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.25, 0.25, 0.25, 0.25));
    let mut ledger = UnifiedEvidenceLedger::new(500);
    let mut logs = Vec::new();
    let mut frame = 0u64;

    for &(regime, count) in &[
        (Regime::Stable, 20),
        (Regime::Bursty, 20),
        (Regime::Resize, 20),
        (Regime::Degraded, 20),
    ] {
        run_frames(&mut ctrl, &mut ledger, regime, count, frame, &mut logs);
        frame += count;
    }

    for log in &logs {
        let chosen_loss = log.expected_losses[action_index(log.action_chosen)];
        for (i, &loss) in log.expected_losses.iter().enumerate() {
            assert!(
                chosen_loss <= loss + 1e-10,
                "Frame {}: chosen action {:?} (loss {:.6}) > alternative {} (loss {:.6})",
                log.frame_id,
                log.action_chosen,
                chosen_loss,
                i,
                loss,
            );
        }
    }
}

/// Decision count tracking is accurate.
#[test]
fn decision_and_calibration_counts() {
    let mut ctrl = RenderController::new(RegimePosterior::new(0.7, 0.1, 0.1, 0.1));
    let mut ledger = UnifiedEvidenceLedger::new(200);
    let mut logs = Vec::new();

    run_frames(&mut ctrl, &mut ledger, Regime::Stable, 42, 0, &mut logs);

    assert_eq!(ctrl.decision_count, 42);
    assert_eq!(ctrl.calibration_count, 42);
}
